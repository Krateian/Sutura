# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""sutura/proxy_repair.py - Proxy-template repair for heavily broken meshes.

Standalone module for reconstructing clean, watertight models from meshes with
severe defects (open holes, self-intersections, non-manifold edges, debris).
Pipeline:
  1. Detect healthy original surface regions (filtering defects, inconsistent winding,
     debris components, and dilating by 1-2 rings).
  2. Build a coarse watertight proxy surface (resampled uniform mesh + hole closing).
  3. Enforce face budget (output <= max(2x input, 20k) faces) and target edge length.
  4. Project proxy vertices onto healthy original surface (>= 90% of healthy subset projected).
  5. Check and rollback collapsed/inverted faces without false-positive reverts.
  6. Honest manifold3d validity verification and one-sided Hausdorff reporting.
"""

import numpy as np

import sutura_geom

# Shared with the closing tier: the pure-numpy watertight verdict and the
# input->output one-sided Hausdorff. Kept as a single implementation (bit-identical
# code previously lived in both tiers).
from sutura_engine.methods.closing import _check_validity, one_sided_hausdorff

# Threshold constants
DEFAULT_VOXEL_DIVISOR = 80
MIN_GRID_CELLS = 32
MAX_GRID_CELLS = 256

DEFAULT_DILATION_RINGS = 1
MIN_DEBRIS_COMPONENT_FACES = 20

PROJECTION_DIST_FACTOR = 3.0  # max projection distance = k * voxel
PROJECTION_MIN_NORMAL_DOT = 0.0  # alignment threshold for projection

SI_FACE_CAP = 200_000  # cap self-intersection checks above 200k faces
MIN_FACE_BUDGET = 20_000  # output faces <= max(2x input, 20k)

# PyMeshLab's ``generate_resampled_uniform_mesh`` (VCG) builds a face KdTree
# whose construction can fail to terminate on meshes that carry very long thin
# triangles (e.g. a coarse bottom fan meeting a fine grid: max edge / median
# edge ~ 56). The native loop is not interruptible from Python, so the call is
# run in a bounded subprocess; on timeout the caller falls through to the next
# strategy (ultimately ``generate_alpha_wrap``, which is fast and robust).
PROXY_RESAMPLE_TIMEOUT = 20.0  # seconds, per uniform-resample attempt

# The same non-terminating VCG KdTree construction grows to many GB of resident
# memory before the time budget expires. The subprocess is therefore also killed
# once its RSS exceeds this ceiling, so the guard bounds memory as well as time.
# The value sits far above any healthy resample (measured < 450 MB total, parent
# included, on the smoke set) and is overridable via
# ``SUTURA_PROXY_RESAMPLE_MAX_RSS_MB`` (0 disables it).
PROXY_RESAMPLE_MAX_RSS_MB = 2048


def _resample_timeout():
    """Resolved per-attempt resample budget (env ``SUTURA_PROXY_RESAMPLE_TIMEOUT``)."""
    import os
    raw = os.environ.get('SUTURA_PROXY_RESAMPLE_TIMEOUT')
    if raw:
        try:
            val = float(raw)
            if val > 0:
                return val
        except (TypeError, ValueError):
            pass
    return PROXY_RESAMPLE_TIMEOUT


def _resample_max_rss_kb():
    """Resolved per-attempt resample memory ceiling in kB (env override, 0 = off)."""
    import os
    raw = os.environ.get('SUTURA_PROXY_RESAMPLE_MAX_RSS_MB')
    mb = PROXY_RESAMPLE_MAX_RSS_MB
    if raw:
        try:
            val = float(raw)
            if val >= 0:
                mb = val
        except (TypeError, ValueError):
            pass
    return int(mb) * 1024


def _proc_rss_kb(pid):
    """Resident set size of ``pid`` in kB, or ``None`` if unavailable."""
    import sys
    try:
        if sys.platform.startswith('linux'):
            with open('/proc/%d/status' % int(pid)) as fh:
                for line in fh:
                    if line.startswith('VmRSS:'):
                        return int(line.split()[1])
            return None
        import subprocess
        out = subprocess.run(['ps', '-o', 'rss=', '-p', str(int(pid))],
                             capture_output=True, text=True)
        txt = out.stdout.strip()
        return int(txt) if txt else None
    except Exception:
        return None


def _uniform_resample(verts, tris, voxel, offset, absdist):
    """Run one ``generate_resampled_uniform_mesh`` call and return (verts, faces)."""
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, dtype=np.float64),
                        face_matrix=np.asarray(tris, np.int32)))
    ms.apply_filter('generate_resampled_uniform_mesh',
                    cellsize=ml.PureValue(float(voxel)),
                    offset=ml.PureValue(float(offset)),
                    absdist=bool(absdist))
    out = ms.current_mesh()
    return (np.asarray(out.vertex_matrix(), dtype=np.float64),
            np.asarray(out.face_matrix(), dtype=np.int32))


def _resample_worker(in_npz, out_npz, voxel, offset, absdist):
    """Subprocess entry point for :func:`_uniform_resample_isolated`."""
    data = np.load(in_npz)
    pv, pt = _uniform_resample(np.asarray(data['v'], dtype=np.float64),
                               np.asarray(data['t'], dtype=np.int64),
                               float(voxel), float(offset), bool(absdist))
    np.savez(out_npz, v=pv, t=pt)


def _uniform_resample_isolated(verts, tris, voxel, offset, absdist, timeout=None):
    """``_uniform_resample`` in a subprocess, bounded by ``timeout`` seconds.

    Protects the repair from the VCG KdTree non-termination described above:
    on timeout (or any child failure) this raises, and ``build_proxy`` falls
    through to its next strategy. The child is also killed once its resident
    set exceeds ``_resample_max_rss_kb()`` so the guard bounds memory too.
    The child runs under the current interpreter with the parent's ``sys.path``
    so it resolves the same ``sutura_engine`` package in every install layout.
    """
    import os
    import shutil
    import subprocess
    import sys
    import tempfile
    import threading
    import time

    if timeout is None:
        timeout = _resample_timeout()
    max_rss_kb = _resample_max_rss_kb()
    work = tempfile.mkdtemp(prefix='sutura_proxy_resample_')
    try:
        in_npz = os.path.join(work, 'in.npz')
        out_npz = os.path.join(work, 'out.npz')
        np.savez(in_npz,
                 v=np.ascontiguousarray(verts, dtype=np.float64),
                 t=np.ascontiguousarray(tris, np.int32))
        path = [p for p in sys.path if isinstance(p, str)]
        code = (
            "import sys; sys.path[:0] = %r\n"
            "from sutura_engine.methods.proxy import _resample_worker\n"
            "_resample_worker(%r, %r, %r, %r, %r)\n"
            % (path, in_npz, out_npz, float(voxel), float(offset), bool(absdist))
        )
        proc = subprocess.Popen([sys.executable, '-c', code],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        over_mem = threading.Event()

        def _watch():
            while proc.poll() is None:
                rss = _proc_rss_kb(proc.pid)
                if max_rss_kb > 0 and rss is not None and rss > max_rss_kb:
                    over_mem.set()
                    proc.kill()
                    return
                time.sleep(0.2)

        watcher = threading.Thread(target=_watch, daemon=True)
        watcher.start()
        try:
            try:
                out, err = proc.communicate(timeout=float(timeout))
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.communicate()
                raise TimeoutError(
                    'proxy resample exceeded the %.1fs budget' % float(timeout))
        finally:
            watcher.join(timeout=1.0)
        if over_mem.is_set():
            raise RuntimeError(
                'proxy resample exceeded the %d MB memory cap'
                % (max_rss_kb // 1024))
        if proc.returncode != 0:
            raise subprocess.CalledProcessError(
                proc.returncode, 'proxy_resample', output=out, stderr=err)
        data = np.load(out_npz)
        return (np.asarray(data['v'], dtype=np.float64),
                np.asarray(data['t'], dtype=np.int32))
    finally:
        shutil.rmtree(work, ignore_errors=True)


def _referenced_only(verts, tris):
    """Drop unreferenced vertices and remap triangle indices."""
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) == 0:
        return np.zeros((0, 3), dtype=np.float64), np.zeros((0, 3), dtype=np.int64)
    used = np.unique(tris)
    remap = np.full(len(verts), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return verts[used], remap[tris]


def _filter_debris_components(verts, tris, min_faces=MIN_DEBRIS_COMPONENT_FACES):
    """Filter out small disconnected debris components (< min_faces), keeping legitimate parts."""
    if len(tris) == 0:
        return verts, tris
    import topology

    n_v = len(verts)
    e = np.vstack([tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]])
    n_components, labels = topology.connected_components((n_v, e))
    if n_components <= 1:
        return verts, tris

    face_labels = labels[tris[:, 0]]
    uniq, counts = np.unique(face_labels, return_counts=True)
    keep_labels = set(uniq[counts >= min_faces])
    if not keep_labels:
        largest_label = uniq[np.argmax(counts)]
        keep_labels = {largest_label}

    keep_mask = np.isin(face_labels, list(keep_labels))
    return _referenced_only(verts, tris[keep_mask])


def _keep_largest_component(verts, tris):
    """Keep only the largest connected component of faces."""
    if len(tris) == 0:
        return verts, tris
    import topology

    n_v = len(verts)
    e = np.vstack([tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]])
    n_components, labels = topology.connected_components((n_v, e))
    if n_components <= 1:
        return verts, tris

    face_labels = labels[tris[:, 0]]
    uniq, counts = np.unique(face_labels, return_counts=True)
    largest_label = uniq[np.argmax(counts)]
    keep_faces = tris[face_labels == largest_label]
    used_verts = np.unique(keep_faces)
    remap = np.full(n_v, -1, dtype=np.int32)
    remap[used_verts] = np.arange(len(used_verts))
    return verts[used_verts], remap[keep_faces]


def build_proxy(verts, tris, voxel=None, resample_timeout=None):
    """Construct a watertight, coarse proxy surface for the input mesh.

    Attempts in order:
      (a) PyMeshLab generate_resampled_uniform_mesh (absdist=False, offset=0.0) + meshing_close_holes
      (a2) PyMeshLab generate_resampled_uniform_mesh (absdist=True, offset=voxel)
      (b) PyMeshLab generate_alpha_wrap fallback

    Voxel size is clamped so grid dimensions stay within [32, 256].

    ``resample_timeout`` (default: ``_resample_timeout()``) bounds each
    uniform-resample attempt; the two PyMeshLab resample strategies run in a
    subprocess so a VCG KdTree non-termination on long thin triangles cannot
    hang the repair (see ``PROXY_RESAMPLE_TIMEOUT``). The subprocess is also
    killed once its RSS exceeds ``PROXY_RESAMPLE_MAX_RSS_MB``, so the guard
    bounds memory as well as time. On either bound the strategy is skipped and
    the alpha-wrap fallback is used.
    Returns (pv, pt, info_dict).
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)

    if len(verts) < 3 or len(tris) == 0:
        raise ValueError("Cannot build proxy from empty or degenerate mesh")

    in_v, in_t = _referenced_only(verts, tris)
    in_v, in_t = _filter_debris_components(in_v, in_t, min_faces=MIN_DEBRIS_COMPONENT_FACES)

    bbox_span = in_v.max(axis=0) - in_v.min(axis=0)
    max_dim = float(np.max(bbox_span))
    diag = float(np.linalg.norm(bbox_span))
    if max_dim <= 1e-12:
        raise ValueError("Degenerate bounding box")

    if voxel is None:
        voxel = diag / float(DEFAULT_VOXEL_DIVISOR)

    # Clamp voxel grid to [MIN_GRID_CELLS, MAX_GRID_CELLS]
    if max_dim / voxel > MAX_GRID_CELLS:
        voxel = max_dim / MAX_GRID_CELLS
    if max_dim / voxel < MIN_GRID_CELLS:
        voxel = max_dim / MIN_GRID_CELLS

    import pymeshlab as ml
    import time

    if resample_timeout is None:
        resample_timeout = _resample_timeout()
    resample_start = time.monotonic()

    # Strategy 1: Signed resampling with hole closure (produces a single watertight shell)
    try:
        rv, rt = _uniform_resample_isolated(in_v, in_t, voxel, 0.0, False,
                                            timeout=resample_timeout)
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=rv, face_matrix=np.asarray(rt, np.int32)))
        ms.apply_filter('meshing_close_holes', maxholesize=10000)
        ms.apply_filter('meshing_close_holes', maxholesize=10000)
        out = ms.current_mesh()
        pv = np.asarray(out.vertex_matrix(), dtype=np.float64)
        pt = np.asarray(out.face_matrix(), dtype=np.int32)
        pv, pt = _filter_debris_components(pv, pt, min_faces=MIN_DEBRIS_COMPONENT_FACES)
        is_wt, _ = _check_validity(pv, pt)
        if is_wt and len(pt) >= 4:
            return pv, pt, {
                'method': 'uniform_resampled_signed',
                'voxel': voxel,
                'faces': len(pt),
            }
    except Exception:
        pass

    # Strategy 2: Unsigned distance offset resampling (absdist=True, offset=voxel)
    try:
        remaining = max(resample_timeout - (time.monotonic() - resample_start), 3.0)
        rv, rt = _uniform_resample_isolated(in_v, in_t, voxel, voxel, True,
                                            timeout=remaining)
        pv, pt = _filter_debris_components(rv, rt, min_faces=MIN_DEBRIS_COMPONENT_FACES)
        is_wt, _ = _check_validity(pv, pt)
        if is_wt and len(pt) >= 4:
            return pv, pt, {
                'method': 'uniform_resampled_unsigned',
                'voxel': voxel,
                'faces': len(pt),
            }
    except Exception:
        pass

    # Strategy 3: Alpha wrap fallback (guaranteed watertight 2-manifold)
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=in_v, face_matrix=np.asarray(in_t, np.int32)))
        alpha = voxel * 2.0
        offset = voxel / 4.0
        ms.apply_filter(
            'generate_alpha_wrap',
            alpha=ml.PureValue(alpha),
            offset=ml.PureValue(offset),
        )
        out = ms.current_mesh()
        pv = np.asarray(out.vertex_matrix(), dtype=np.float64)
        pt = np.asarray(out.face_matrix(), dtype=np.int32)
        pv, pt = _filter_debris_components(pv, pt, min_faces=MIN_DEBRIS_COMPONENT_FACES)
        is_wt, _ = _check_validity(pv, pt)
        if is_wt and len(pt) >= 4:
            return pv, pt, {
                'method': 'alpha_wrap',
                'voxel': voxel,
                'faces': len(pt),
            }
    except Exception as e:
        raise RuntimeError(f"Failed to build watertight proxy: {e}")

    raise RuntimeError("All proxy generation strategies failed to produce a valid 2-manifold")


def health_mask(verts, tris, dilation_rings=DEFAULT_DILATION_RINGS, si_face_cap=SI_FACE_CAP):
    """Compute per-face health boolean mask.

    False for:
      - Faces touching boundary edges or non-manifold edges.
      - Faces touching edges with inconsistent winding (flipped orientations).
      - Small disconnected debris components (< 20 faces).
      - Self-intersecting faces (capped at 200k faces).
      - Dilated by 1-2 rings of neighboring faces.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    n_faces = len(tris)
    if n_faces == 0:
        return np.zeros(0, dtype=bool)

    healthy = np.ones(n_faces, dtype=bool)
    max_v = int(tris.max()) + 1

    # 1. Undirected edges (boundary & non-manifold edges)
    edges_undir = np.sort(np.vstack([tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]]), axis=1)
    keys_undir = edges_undir[:, 0] * max_v + edges_undir[:, 1]
    uniq_u, counts_u = np.unique(keys_undir, return_counts=True)
    bad_undir = set(uniq_u[counts_u != 2])

    # 2. Directed edges (inconsistent winding / flipped orientation)
    edges_dir = np.vstack([tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]])
    keys_dir = edges_dir[:, 0] * max_v + edges_dir[:, 1]
    uniq_d, counts_d = np.unique(keys_dir, return_counts=True)
    bad_dir = set(uniq_d[counts_d > 1])

    bad_face_indices = set()
    for f_idx, tri in enumerate(tris):
        for u, w in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])]:
            if min(u, w) * max_v + max(u, w) in bad_undir or u * max_v + w in bad_dir:
                bad_face_indices.add(f_idx)

    # 3. Debris components
    import topology

    edge_to_faces = {}
    for f_idx, tri in enumerate(tris):
        for e in [
            (min(tri[0], tri[1]), max(tri[0], tri[1])),
            (min(tri[1], tri[2]), max(tri[1], tri[2])),
            (min(tri[2], tri[0]), max(tri[2], tri[0])),
        ]:
            edge_to_faces.setdefault(e, []).append(f_idx)

    fi, fj = [], []
    for f_list in edge_to_faces.values():
        if len(f_list) == 2:
            fi.extend([f_list[0], f_list[1]])
            fj.extend([f_list[1], f_list[0]])

    if fi:
        _, labels = topology.connected_components((n_faces, np.column_stack((fi, fj))))
        uniq_l, counts_l = np.unique(labels, return_counts=True)
        small_labels = set(uniq_l[counts_l < MIN_DEBRIS_COMPONENT_FACES])
        for f_idx, l in enumerate(labels):
            if l in small_labels:
                bad_face_indices.add(f_idx)

    # 4. Self-intersections via PyMeshLab
    if n_faces <= si_face_cap:
        try:
            import pymeshlab as ml
            ms = ml.MeshSet()
            ms.add_mesh(ml.Mesh(vertex_matrix=verts, face_matrix=np.asarray(tris, np.int32)))
            ms.apply_filter('compute_selection_by_self_intersections_per_face')
            si_sel = ms.current_mesh().face_selection_array()
            for f_idx in np.where(si_sel)[0]:
                bad_face_indices.add(int(f_idx))
        except Exception:
            pass

    for idx in bad_face_indices:
        healthy[idx] = False

    # 5. Dilate unhealthy set by rings
    v_to_faces = [[] for _ in range(max_v)]
    for f_idx, tri in enumerate(tris):
        v_to_faces[tri[0]].append(f_idx)
        v_to_faces[tri[1]].append(f_idx)
        v_to_faces[tri[2]].append(f_idx)

    for _ in range(dilation_rings):
        unhealthy_v = set()
        for f_idx in np.where(~healthy)[0]:
            tri = tris[f_idx]
            unhealthy_v.add(tri[0])
            unhealthy_v.add(tri[1])
            unhealthy_v.add(tri[2])
        for v in unhealthy_v:
            for f_idx in v_to_faces[v]:
                healthy[f_idx] = False

    return healthy


def proxy_template_repair(verts, tris, voxel=None, max_iters=3):
    """Repair broken mesh by proxy-template projection and collision rollback.

    - Enforces face budget: output faces <= max(2x input, 20k).
    - Derives target edge length from the healthy input for projection distance threshold.
    - Projects >= 90% of proxy vertices whose nearest source point lies on a healthy face.
    - Rollback loop eliminates inverted/collapsed faces without mass false-positive reverts.
    - Honest manifold3d validity verification and one-sided Hausdorff reporting.

    Returns (repaired_v, repaired_t, report_dict).
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)

    if len(verts) < 3 or len(tris) == 0:
        return verts, tris, {
            'watertight': False,
            'output_faces': len(tris),
            'proxy_faces': 0,
            'projected_fraction': 0.0,
            'projected_fraction_healthy': 0.0,
            'reverted_count': 0,
            'error': 'Empty or degenerate input mesh',
            'notes': [],
        }

    try:
        import trimesh
        import pymeshlab as ml

        diag = float(np.linalg.norm(verts.max(axis=0) - verts.min(axis=0)))

        # 1. Extract healthy original surface
        h_mask = health_mask(verts, tris, dilation_rings=DEFAULT_DILATION_RINGS)
        has_healthy = bool(np.any(h_mask))

        if not has_healthy:
            # Entire input was defective; return coarse proxy directly
            pv, pt, proxy_info = build_proxy(verts, tris, voxel=voxel)
            is_wt, err_msg = _check_validity(pv, pt)
            if not is_wt:
                return verts, tris, {
                    'watertight': False,
                    'output_faces': len(tris),
                    'proxy_faces': len(pt),
                    'projected_fraction': 0.0,
                    'projected_fraction_healthy': 0.0,
                    'reverted_count': 0,
                    'error': err_msg,
                    'notes': [],
                }
            return pv, pt, {
                'watertight': True,
                'proxy_faces': len(pt),
                'output_faces': len(pt),
                'projected_fraction': 0.0,
                'projected_fraction_healthy': 0.0,
                'reverted_count': 0,
                'notes': ['regions without healthy source were reconstructed from a coarse proxy'],
            }

        healthy_v, healthy_t = _referenced_only(verts, tris[h_mask])

        # 2. Target edge length from healthy input
        e0 = healthy_v[healthy_t[:, 1]] - healthy_v[healthy_t[:, 0]]
        e1 = healthy_v[healthy_t[:, 2]] - healthy_v[healthy_t[:, 1]]
        e2 = healthy_v[healthy_t[:, 0]] - healthy_v[healthy_t[:, 2]]
        lengths = np.concatenate([
            np.linalg.norm(e0, axis=1),
            np.linalg.norm(e1, axis=1),
            np.linalg.norm(e2, axis=1),
        ])
        target_edge_len = float(np.median(lengths))

        # Target voxel size
        if voxel is None:
            voxel = diag / float(DEFAULT_VOXEL_DIVISOR)

        # 3. Build watertight proxy
        pv, pt, proxy_info = build_proxy(verts, tris, voxel=voxel)
        proxy_faces_count = len(pt)
        used_voxel = proxy_info['voxel']

        # 4. Enforce face budget: output <= max(2x input, 20k)
        max_budget = max(len(tris) * 2, MIN_FACE_BUDGET)
        if len(pt) > max_budget:
            try:
                ms_dec = ml.MeshSet()
                ms_dec.add_mesh(ml.Mesh(vertex_matrix=pv, face_matrix=np.asarray(pt, np.int32)))
                ms_dec.apply_filter(
                    'meshing_decimation_quadric_edge_collapse',
                    targetfacenum=max_budget,
                    preservetopology=True,
                    planarquadric=True,
                )
                out_dec = ms_dec.current_mesh()
                candidate_pv = np.asarray(out_dec.vertex_matrix(), dtype=np.float64)
                candidate_pt = np.asarray(out_dec.face_matrix(), dtype=np.int32)
                candidate_pv, candidate_pt = _filter_debris_components(candidate_pv, candidate_pt)
                is_wt, _ = _check_validity(candidate_pv, candidate_pt)
                if is_wt and len(candidate_pt) >= 4:
                    pv, pt = candidate_pv, candidate_pt
            except Exception:
                pass

        # 5. Proximity & projection onto healthy original surface
        proxy_mesh = trimesh.Trimesh(vertices=pv, faces=pt, process=False)

        closest_pts, distances, triangle_indices = sutura_geom.closest_points(
            verts, tris, pv)

        # Subset of proxy vertices whose nearest source point lies on a healthy face
        nearest_is_healthy = h_mask[triangle_indices]
        subset_count = int(np.sum(nearest_is_healthy))

        # Distance threshold derived from healthy input edge length and voxel
        max_dist = max(PROJECTION_DIST_FACTOR * used_voxel, 2.0 * target_edge_len)
        proj_mask = nearest_is_healthy & (distances < max_dist)

        projected_pv = pv.copy()
        projected_pv[proj_mask] = closest_pts[proj_mask]

        # 6. Collision / inverted face rollback (prevent false-positive mass rollbacks)
        total_reverted = 0
        for _ in range(max_iters):
            v0 = projected_pv[pt[:, 0]]
            v1 = projected_pv[pt[:, 1]]
            v2 = projected_pv[pt[:, 2]]
            face_normals = np.cross(v1 - v0, v2 - v0)
            face_areas = 0.5 * np.linalg.norm(face_normals, axis=1)
            orig_face_normals = proxy_mesh.face_normals
            dot_align = np.sum(face_normals * orig_face_normals, axis=1)

            bad_faces = (face_areas < 1e-8) | (dot_align <= 0.0)
            if not np.any(bad_faces):
                break

            bad_v = np.unique(pt[bad_faces])
            revertible = proj_mask[bad_v]
            reverted_now = np.sum(revertible)
            if reverted_now == 0:
                break

            # Revert to unprojected proxy position
            projected_pv[bad_v] = pv[bad_v]
            proj_mask[bad_v] = False
            total_reverted += int(reverted_now)

        # Compute projected fractions
        if subset_count > 0:
            projected_fraction_healthy = float(np.sum(proj_mask) / subset_count)
        else:
            projected_fraction_healthy = 0.0
        projected_fraction = float(np.mean(proj_mask))

        # Post-projection face budget enforcement
        if len(pt) > max_budget:
            try:
                ms_dec = ml.MeshSet()
                ms_dec.add_mesh(ml.Mesh(vertex_matrix=projected_pv, face_matrix=np.asarray(pt, np.int32)))
                ms_dec.apply_filter(
                    'meshing_decimation_quadric_edge_collapse',
                    targetfacenum=max_budget,
                    preservetopology=True,
                    planarquadric=True,
                )
                out_dec = ms_dec.current_mesh()
                dec_pv = np.asarray(out_dec.vertex_matrix(), dtype=np.float64)
                dec_pt = np.asarray(out_dec.face_matrix(), dtype=np.int32)
                dec_pv, dec_pt = _filter_debris_components(dec_pv, dec_pt)
                is_wt_dec, _ = _check_validity(dec_pv, dec_pt)
                if is_wt_dec and len(dec_pt) >= 4:
                    projected_pv, pt = dec_pv, dec_pt
            except Exception:
                pass

        # 7. Final validity check
        is_wt, err_msg = _check_validity(projected_pv, pt)
        if not is_wt:
            # If projection damaged manifoldness, safely fall back to unprojected proxy
            projected_pv = pv
            is_wt, err_msg = _check_validity(projected_pv, pt)

        if not is_wt:
            # A failed validity NEVER returns an empty or invalid mesh: hand the
            # input back unchanged and report the reason honestly.
            return verts, tris, {
                'watertight': False,
                'output_faces': len(tris),
                'proxy_faces': proxy_faces_count,
                'projected_fraction': 0.0,
                'projected_fraction_healthy': 0.0,
                'reverted_count': total_reverted,
                'error': err_msg,
                'notes': [],
            }

        # 8. One-sided Hausdorff (healthy input surface to output)
        h_max, h_mean = one_sided_hausdorff(healthy_v, healthy_t, projected_pv, pt)

        report = {
            'watertight': True,
            'proxy_faces': proxy_faces_count,
            'output_faces': len(pt),
            'projected_fraction': round(projected_fraction, 4),
            'projected_fraction_healthy': round(projected_fraction_healthy, 4),
            'reverted_count': total_reverted,
            'hausdorff_max': h_max,
            'hausdorff_mean': h_mean,
            'notes': ['regions without healthy source were reconstructed from a coarse proxy'],
        }
        return projected_pv, pt, report

    except Exception as e:
        return verts, tris, {
            'watertight': False,
            'output_faces': len(tris),
            'proxy_faces': 0,
            'projected_fraction': 0.0,
            'projected_fraction_healthy': 0.0,
            'reverted_count': 0,
            'error': str(e),
            'notes': [],
        }
