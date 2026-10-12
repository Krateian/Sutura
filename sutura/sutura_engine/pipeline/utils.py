# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Pure geometry/topology helpers shared by the repair tiers.

Moved verbatim from sutura/repair.py (P1a split); no logic change. These are
stdlib + numpy only (the two SI helpers lazy-import ``sutura_geom``), so the
module stays importable without pymeshlab.
"""
from collections import defaultdict

import numpy as np

import topology


def surface_area(verts, tris):
    """Total triangle surface area of a mesh, computed directly from geometry.

    Works for both open and closed meshes (VCG's geometric-measures volume
    and area are only meaningful on closed meshes). Pure numpy, platform
    independent."""
    if len(tris) == 0 or len(verts) == 0:
        return 0.0
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    a = v[t[:, 0]]
    b = v[t[:, 1]]
    c = v[t[:, 2]]
    cross = np.cross(b - a, c - a)
    return float(np.sum(0.5 * np.linalg.norm(cross, axis=1)))


def boundary_loop_stats(verts, tris):
    """(n_loops, max_loop_len) of the mesh boundary: walks the boundary-edge
    graph (edges used by exactly one face). A closed mesh returns (0, 0).
    Pure numpy + dict (no pymeshlab); each boundary vertex of a 2-manifold
    mesh has degree 2, so loops = connected boundary components."""
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) == 0:
        return 0, 0
    f0, f1, f2 = tris[:, 0], tris[:, 1], tris[:, 2]
    V = int(tris.max()) + 1
    keys = np.concatenate([
        np.minimum(f0, f1) * V + np.maximum(f0, f1),
        np.minimum(f1, f2) * V + np.maximum(f1, f2),
        np.minimum(f0, f2) * V + np.maximum(f0, f2),
    ]).astype(np.int64)
    uniq, counts = np.unique(keys, return_counts=True)
    bkeys = uniq[counts == 1]
    if len(bkeys) == 0:
        return 0, 0
    ba, bb = bkeys // V, bkeys % V
    adj = defaultdict(list)
    for a, c in zip(ba.tolist(), bb.tolist()):
        adj[a].append(c)
        adj[c].append(a)
    seen, loops = set(), []
    for s in adj:
        if s in seen:
            continue
        n, cur, prev = 0, s, None
        while cur not in seen:
            seen.add(cur)
            n += 1
            nxt = [x for x in adj[cur] if x != prev]
            if not nxt:
                break
            prev, cur = cur, nxt[0]
        loops.append(n)
    return len(loops), (max(loops) if loops else 0)


def signed_volume(verts, tris):
    """Signed volume of a triangle mesh (sum of origin-tetrahedra volumes).

    The sign reflects the winding orientation: a negative value means the
    faces are globally inverted (normals pointing inward). Pure numpy, works
    for open meshes too (the value is then not a real volume, only a signed
    sum - interpret with care)."""
    if len(tris) == 0 or len(verts) == 0:
        return 0.0
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    a = v[t[:, 0]]
    b = v[t[:, 1]]
    c = v[t[:, 2]]
    return float(np.sum(np.einsum('ij,ij->i', a, np.cross(b, c))) / 6.0)


def _edge_uses(tris):
    """Undirected edges of ``tris`` as (keys, counts, V) with key = a*V+b."""
    tris = np.asarray(tris, dtype=np.int64)
    V = int(tris.max()) + 1
    a = np.concatenate([tris[:, 0], tris[:, 1], tris[:, 2]])
    b = np.concatenate([tris[:, 1], tris[:, 2], tris[:, 0]])
    keys = np.minimum(a, b) * V + np.maximum(a, b)
    uniq, inv, counts = np.unique(keys, return_inverse=True, return_counts=True)
    return keys, uniq, inv, counts


def _faces_preserved(verts, tris, new_verts, new_tris):
    """True when every face of (verts, tris) occurs in (new_verts,
    new_tris) with bit-identical float32 vertex coordinates and the same
    orientation, counted with multiplicity (index- and order-independent,
    a face may start at any of its three corners)."""
    a = np.asarray(verts, dtype=np.float32)[np.asarray(tris, dtype=np.int64)]
    b = np.asarray(new_verts, dtype=np.float32)[np.asarray(new_tris, dtype=np.int64)]
    pts = np.ascontiguousarray(np.concatenate([a.reshape(-1, 3), b.reshape(-1, 3)]))
    _u, ids = np.unique(pts.view(np.dtype((np.void, 12))).ravel(), return_inverse=True)
    ids = ids.reshape(-1, 3)
    fa, fb = ids[:len(a)], ids[len(a):]

    def canon(f):
        # lexicographically smallest cyclic rotation (also well defined when
        # two corners share a coordinate)
        best = f
        for k in (1, 2):
            r = np.roll(f, -k, axis=1)
            less = ((r[:, 0] < best[:, 0])
                    | ((r[:, 0] == best[:, 0])
                       & ((r[:, 1] < best[:, 1])
                          | ((r[:, 1] == best[:, 1]) & (r[:, 2] < best[:, 2])))))
            best = np.where(less[:, None], r, best)
        return best
    ua, ca = np.unique(canon(fa), axis=0, return_counts=True)
    ub, cb = np.unique(canon(fb), axis=0, return_counts=True)
    if len(ua) == 0:
        return True
    # count of each needed face among the available ones
    both = np.concatenate([ub, ua])
    _uu, inv = np.unique(both, axis=0, return_inverse=True)
    inv = inv.ravel()
    have = np.zeros(len(_uu), dtype=np.int64)
    have[inv[:len(ub)]] = cb
    return bool(np.all(have[inv[len(ub):]] >= ca))


def _referenced_only(verts, tris):
    """(verts, tris) without the vertices no face references (float64)."""
    tris = np.asarray(tris, dtype=np.int64)
    used = np.unique(tris)
    remap = np.full(len(verts), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return np.asarray(verts, np.float64)[used], remap[tris]


def _count_si(ml, ms):
    if topology.native_si_enabled():
        m = ms.current_mesh()
        _, count = topology.self_intersecting_faces(m.vertex_matrix(), m.face_matrix())
        return max(int(count), 0)
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    n = int(ms.current_mesh().face_selection_array().sum())
    ms.apply_filter('set_selection_none')
    return n


def _exact_si_face_count(verts, tris):
    """Exact proper-crossing self-intersection face count via the Rust core.

    Returns ``None`` when the classifier is unavailable or skipped (mesh above
    the classifier's face cap); the same metric the independent verifier uses.
    """
    try:
        import sutura_geom
    except Exception:  # noqa: BLE001 - no extension is a normal case
        return None
    if not hasattr(sutura_geom, 'self_intersecting_faces'):
        return None
    try:
        t = np.asarray(tris, dtype=np.int32)
        if len(t) == 0:
            return 0
        _mask, count = sutura_geom.self_intersecting_faces(
            np.asarray(verts, dtype=np.float64), t)
        return None if count < 0 else int(count)
    except Exception:  # noqa: BLE001 - a classifier failure is not fatal
        return None


def _internal_wall_count(verts, tris):
    """Count buried "internal wall" faces of a mesh.

    A face is an internal wall when its centroid reads a generalized winding
    number ``>= 1.5`` on BOTH sides (both sides are inside the solid), i.e. the
    surface is buried inside other geometry.  Used by the Tamponade adoption
    gate as a never-worse metric relative to the tier's own input (a mesh can
    already legitimately carry internal walls, so an absolute ``== 0`` test
    would reject an otherwise no-op candidate forever).  Returns 0 when the
    Rust extension is unavailable.
    """
    try:
        from sutura_geom import PyMeshBvh
        v = np.asarray(verts, dtype=np.float64)
        t = np.asarray(tris, dtype=np.int32)
        if len(t) == 0 or len(v) == 0:
            return 0
        bvh = PyMeshBvh(v, t)
        p0 = v[t[:, 0]]
        p1 = v[t[:, 1]]
        p2 = v[t[:, 2]]
        c = (p0 + p1 + p2) / 3.0
        n = np.cross(p1 - p0, p2 - p0)
        ln = np.linalg.norm(n, axis=1, keepdims=True)
        ln[ln == 0] = 1.0
        n = n / ln
        diag = float(np.linalg.norm(v.max(0) - v.min(0)))
        eps = max(diag * 1e-4, 1e-6)
        wp = bvh.winding_points(c + n * eps)
        wm = bvh.winding_points(c - n * eps)
        return int(((wp >= 1.5) & (wm >= 1.5)).sum())
    except Exception:  # noqa: BLE001 - a metric failure is not fatal
        return 0


def _canonical_face_keys(verts, tris):
    """Order-independent ``V36`` key per face (its 3 vertex coords, sorted).

    Coordinates are rounded to ``float32`` (the on-disk STL precision) and the
    three vertices of each face are sorted lexicographically, so two faces are
    equal iff they share the same three points regardless of winding or vertex
    numbering.  Used to find the faces a Tamponade candidate changed.
    """
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int64)
    if len(t) == 0 or len(v) == 0:
        return np.zeros(0, dtype='V36')
    q = v[t]
    order = np.lexsort((q[:, :, 2], q[:, :, 1], q[:, :, 0]), axis=1)
    qs = np.take_along_axis(q, order[:, :, None], axis=1)
    qs = np.ascontiguousarray(qs, dtype='<f4')
    return qs.view(np.uint8).reshape(len(qs), 36).view('V36').ravel()


def _bbox_diagonal(verts):
    """Bounding-box diagonal of a vertex array (0.0 for an empty mesh)."""
    if verts is None or len(verts) == 0:
        return 0.0
    a = np.asarray(verts, dtype=np.float64)
    return float(np.linalg.norm(a.max(0) - a.min(0)))


def _count_vertex_components(n_verts, tris):
    """Number of vertex-connected components over the referenced vertices.

    Pure union-find (no scipy) so the Tamponade parts gate stays importable
    anywhere.  Unreferenced vertices are ignored."""
    tris = np.asarray(tris, dtype=np.int64)
    n = int(n_verts)
    if len(tris) == 0 or n == 0:
        return 0
    parent = list(range(n))

    def find(x):
        root = x
        while parent[root] != root:
            root = parent[root]
        while parent[x] != root:
            parent[x], x = root, parent[x]
        return root

    for a, b, c in tris.tolist():
        ra = find(int(a))
        for other in (int(b), int(c)):
            rb = find(other)
            if ra != rb:
                parent[rb] = ra
    used = np.unique(tris)
    return int(len({find(int(i)) for i in used.tolist()}))
