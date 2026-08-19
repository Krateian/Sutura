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
    tri_idx = np.repeat(np.arange(len(tris)), 3)
    edges = np.concatenate([tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]], axis=0)
    keys = _edge_keys(edges)
    uniq, counts = np.unique(keys, return_counts=True)
    boundary = set(int(k) for k in uniq[counts == 1])
    # boundary vertex adjacency: vertex -> [neighbour vertices]
    adj = {}
    for i in range(len(edges)):
        if keys[i] in boundary:
            a, b = int(edges[i][0]), int(edges[i][1])
            adj.setdefault(a, []).append(b)
            adj.setdefault(b, []).append(a)
    return boundary, adj, edges, keys


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
    tri_idx = np.repeat(np.arange(len(tris)), 3)
    edges = np.concatenate([tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]], axis=0)
    keys = _edge_keys(edges)
    uniq, counts = np.unique(keys, return_counts=True)
    nm_keys = set(int(k) for k in uniq[counts > 2])
    if not nm_keys:
        return []
    # faces adjacent to any non-manifold edge, plus edge->faces for clustering
    edge_to_faces = {}
    nm_faces = set()
    for i in range(len(edges)):
        if keys[i] in nm_keys:
            f = int(tri_idx[i])
            nm_faces.add(f)
            edge_to_faces.setdefault(int(keys[i]), set()).add(f)
    nm_faces = sorted(nm_faces)
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

    # faces sharing a non-manifold edge belong to the same region
    for faces in edge_to_faces.values():
        faces = list(faces)
        for i in range(1, len(faces)):
            union(fmap[faces[0]], fmap[faces[i]])
    # also union faces sharing any (not just nm) edge, to merge touching regions
    edge_faces = {}
    for i in range(len(edges)):
        edge_faces.setdefault(int(keys[i]), set()).add(int(tri_idx[i]))
    for faces in edge_faces.values():
        faces = [f for f in faces if f in fmap]
        for i in range(1, len(faces)):
            union(fmap[faces[0]], fmap[faces[i]])

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
