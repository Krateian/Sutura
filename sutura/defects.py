"""Defect detection (hole / non-manifold regions) for the repair heatmap.

stdlib + numpy only, by design: `repair.py` (which runs under the PyMeshLab
venv) imports pymeshlab/trimesh itself and passes plain `verts`/`tris` numpy
arrays in here. This module does pure array work so it stays lightweight and
importable anywhere (same rule as classification.py).

Convention: all coordinates/lengths are in the input mesh's units (STL files
are normally millimetres, but Sutura does not rescale, so callers should treat
values as "mesh units").
"""
import numpy as np

import topology


def _edge_keys(edges):
    """Map each undirected edge (pair of vertex indices) to a scalar key."""
    srt = np.sort(np.asarray(edges, dtype=np.int64), axis=1)
    n = int(srt[:, 1].max()) + 2 if len(srt) else 2
    return srt[:, 0] * n + srt[:, 1]


def _boundary_edges(tris):
    """Return (boundary_key_set, boundary_vertex_adjacency, edges, keys).

    boundary_vertex_adjacency maps each vertex on a boundary to the list of
    vertices it connects to via a boundary edge.
    """
    tris = np.asarray(tris, dtype=np.int64)
    _edges, counts, inv = topology.edge_table(tris)
    if len(tris):
        he = np.concatenate([tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]], axis=0)
    else:
        he = np.zeros((0, 2), dtype=np.int64)
    boundary_pairs = he[counts[inv] == 1] if len(he) else he
    keys = _edge_keys(boundary_pairs)
    boundary = set(int(k) for k in keys)
    # boundary vertex adjacency: vertex -> [neighbour vertices]
    adj = {}
    for a, b in boundary_pairs:
        a, b = int(a), int(b)
        adj.setdefault(a, []).append(b)
        adj.setdefault(b, []).append(a)
    return boundary, adj, boundary_pairs, keys


def _boundary_loops(adj):
    """Trace closed boundary loops from the boundary-vertex adjacency map.
    Returns a list of vertex-index lists."""
    used = set()
    loops = []
    for start in list(adj.keys()):
        if start in used:
            continue
        loop = [start]
        used.add(start)
        cur, prev = start, None
        while True:
            nxt = None
            for nb in adj.get(cur, []):
                if nb == prev:
                    continue
                if nb == start and len(loop) > 2:
                    nxt = nb
                    break
                if nb not in used:
                    nxt = nb
                    break
            if nxt is None or nxt == start:
                break
            used.add(nxt)
            loop.append(nxt)
            prev, cur = cur, nxt
        if len(loop) >= 3:
            loops.append(loop)
    return loops


def _loop_geometry(loop, verts):
    pts = verts[np.asarray(loop, dtype=np.int64)]
    centroid = pts.mean(axis=0).tolist()
    lo = pts.min(axis=0)
    hi = pts.max(axis=0)
    diameter = float(np.linalg.norm(hi - lo))
    return centroid, diameter


def detect_holes(verts, tris, with_indices=False):
    """Return a list of hole dicts:
    {centroid:[x,y,z], diameter, vertices:int}.

    With ``with_indices=True`` each hole also carries ``verts_idx`` (the
    boundary-loop vertex indices) so a renderer can highlight the hole rim.
    The default (False) keeps the CLI JSON contract unchanged."""
    boundary, adj, _edges, _keys = _boundary_edges(tris)
    if not boundary:
        return []
    holes = []
    for loop in _boundary_loops(adj):
        centroid, diameter = _loop_geometry(loop, verts)
        hole = {'centroid': centroid, 'diameter': round(diameter, 4),
                'vertices': len(loop)}
        if with_indices:
            hole['verts_idx'] = [int(i) for i in loop]
        holes.append(hole)
    return holes


def detect_non_manifold(verts, tris, with_indices=False):
    """Return a list of non-manifold region dicts, clustered by face
    connectivity. Each region: {centroid:[x,y,z], faces:int}.

    With ``with_indices=True`` each region also carries ``verts_idx`` (the
    region's vertex indices) and ``faces_idx`` (the region's face indices) so
    a renderer can highlight the region. The default (False) keeps the CLI
    JSON contract unchanged."""
    tris = np.asarray(tris, dtype=np.int64)
    F = len(tris)
    if F == 0:
        return []
    _edges, counts, inv = topology.edge_table(tris)
    nm_ids = np.nonzero(counts > 2)[0]
    if len(nm_ids) == 0:
        return []
    # faces adjacent to any non-manifold edge.  ``edge_table``'s half-edge
    # array is block-stacked ([all (v0,v1), then (v1,v2), then (v2,v0)]), so
    # half-edge ``h`` belongs to face ``h % F`` -- ``np.tile``, NOT
    # ``np.repeat`` (which is face-major, ``h // 3``).
    face_of = np.tile(np.arange(F), 3)
    nm_faces = np.unique(face_of[counts[inv] > 2]).tolist()
    fmap = {f: i for i, f in enumerate(nm_faces)}
    parent = list(range(len(nm_faces)))

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a, b):
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[ra] = rb

    # union faces sharing any edge (not just non-manifold ones) so touching
    # regions merge; only non-manifold faces can be unioned, so restrict to them
    nm_face_bool = np.zeros(F, dtype=bool)
    nm_face_bool[nm_faces] = True
    # same block-stacked layout as ``face_of`` above
    sel = np.nonzero(np.tile(nm_face_bool, 3))[0]
    if len(sel):
        s_inv = inv[sel]
        s_face = face_of[sel]
        order = np.argsort(s_inv, kind='stable')
        s_inv = s_inv[order]
        s_face = s_face[order]
        starts = np.flatnonzero(np.r_[True, s_inv[1:] != s_inv[:-1]])
        ends = np.r_[starts[1:], len(s_inv)]
        for st, en in zip(starts, ends):
            f0 = fmap[int(s_face[st])]
            for k in range(st + 1, en):
                union(f0, fmap[int(s_face[k])])

    groups = {}
    for i, f in enumerate(nm_faces):
        groups.setdefault(find(i), []).append(f)
    regions = []
    for g in groups.values():
        region_verts = np.unique(tris[g].reshape(-1))
        centroid = verts[region_verts].mean(axis=0).tolist()
        region = {'centroid': centroid, 'faces': len(g)}
        if with_indices:
            region['verts_idx'] = [int(i) for i in region_verts]
            region['faces_idx'] = [int(i) for i in g]
        regions.append(region)
    return regions


def detect(verts, tris, with_indices=False):
    """Full defect report: {'holes': [...], 'non_manifold': [...]}.

    ``with_indices`` is passed through to the per-defect detectors; the CLI
    (which calls this without the flag) keeps its JSON contract unchanged,
    while the GUI may request the extra index lists for rendering."""
    return {'holes': detect_holes(verts, tris, with_indices=with_indices),
            'non_manifold': detect_non_manifold(verts, tris,
                                                with_indices=with_indices)}


# Defect-type colours used by the GUI's color-coded mesh view (FAZ11).
_COLOR_DEGENERATE = (250, 210, 60)   # yellow  : degenerate / zero-area face
_COLOR_NON_MANIFOLD = (235, 60, 70)  # red     : non-manifold edge
_COLOR_FLIPPED = (255, 140, 60)      # orange  : flipped (inverted) winding


def defect_type_colors(verts, tris):
    """Per-face defect-type colour array (M,3) uint8 for the input mesh.

    Used by the GUI's color-coded mesh view (FAZ11). Classification:
      yellow  = degenerate (near-zero area, NaN/Inf vertex, repeated indices)
      red     = non-manifold (touches an edge shared by more than two faces)
      orange  = flipped (face normal opposite to its edge-neighbours)
    A face keeps its most severe class (degenerate > non-manifold > flipped);
    (0,0,0) means no defect colour (rendered normally). Pure numpy, stdlib-free,
    does not change ``detect()``'s contract."""
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    F = len(t)
    colors = np.zeros((F, 3), dtype=np.uint8)
    if F == 0 or len(v) == 0:
        return colors
    a = v[t[:, 0]]; b = v[t[:, 1]]; c = v[t[:, 2]]
    cross = np.cross(b - a, c - a)
    areas = 0.5 * np.linalg.norm(cross, axis=1)
    diag = float(np.linalg.norm(v.max(axis=0) - v.min(axis=0))) if len(v) else 1.0
    deg_thresh = 1e-9 * max(diag * diag, 1e-12)
    degenerate = ((areas <= deg_thresh) |
                  ~np.isfinite(areas) |
                  (t[:, 0] == t[:, 1]) | (t[:, 1] == t[:, 2]) |
                  (t[:, 0] == t[:, 2]))

    # non-manifold faces: incident to an edge shared by > 2 faces
    edges = np.concatenate([t[:, [0, 1]], t[:, [1, 2]], t[:, [2, 0]]], axis=0)
    face_of = np.concatenate([np.arange(F), np.arange(F), np.arange(F)])
    emin = np.minimum(edges[:, 0], edges[:, 1])
    emax = np.maximum(edges[:, 0], edges[:, 1])
    V = int(v.shape[0])
    keys = emin * (V + 1) + emax
    uniq, counts = np.unique(keys, return_counts=True)
    nm_keys = set(int(k) for k in uniq[counts > 2])
    nm_face = np.zeros(F, dtype=bool)
    for i in range(len(edges)):
        if keys[i] in nm_keys:
            nm_face[face_of[i]] = True

    # flipped: face normal opposite to the average of its edge-neighbour normals
    mag = np.linalg.norm(cross, axis=1)
    mag[mag == 0] = 1.0
    n = cross / mag[:, None]
    n_deg = np.isfinite(n).all(axis=1)
    n = np.where(n_deg[:, None], n, np.zeros_like(n))
    edge_faces = {}
    for i in range(len(edges)):
        edge_faces.setdefault(int(keys[i]), []).append(int(face_of[i]))
    adj_sum = np.zeros((F, 3))
    adj_cnt = np.zeros(F)
    for faces in edge_faces.values():
        for f1 in faces:
            for f2 in faces:
                if f1 != f2:
                    adj_sum[f1] += n[f2]
                    adj_cnt[f1] += 1
    # flipped: normal clearly OPPOSITE to the neighbours' average, and only
    # when the neighbours are self-consistent (perpendicular neighbours -- e.g.
    # the four faces around a cube face -- average to ~0 and must NOT flag).
    adj_mag = np.linalg.norm(adj_sum, axis=1)
    safe = np.where(adj_mag > 1e-9, 1.0, 1.0)
    cosang = np.sum(n * adj_sum, axis=1) / (adj_mag + 1e-9)
    flipped = ((adj_cnt > 0) & (adj_mag > 1e-6) & (cosang < -0.3) &
               ~degenerate & ~nm_face)

    colors[degenerate] = _COLOR_DEGENERATE
    colors[nm_face & ~degenerate] = _COLOR_NON_MANIFOLD
    colors[flipped] = _COLOR_FLIPPED
    return colors
