"""Defect detection (hole / non-manifold regions) for the repair heatmap.

stdlib + numpy only, by design: `repair.py` (which runs under the PyMeshLab
venv) imports pymeshlab/trimesh itself and passes plain `verts`/`tris` numpy
arrays in here. This module does pure array work so it stays lightweight and
importable anywhere (same rule as classification.py).

Convention: all coordinates/lengths are in the input mesh's units (STL files
are normally millimetres, but Sutura does not rescale, so callers should treat
values as "mesh units").
"""
import math

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
    # Iterate in sorted order so the hole count and loop decomposition do not
    # depend on the face order of the incoming array. Boundary graphs with a
    # vertex of degree > 2 are ambiguous; this makes the chosen decomposition
    # deterministic across equivalent face orderings (W4a parity fix).
    for start in sorted(adj):
        if start in used:
            continue
        loop = [start]
        used.add(start)
        cur, prev = start, None
        while True:
            nxt = None
            for nb in sorted(adj.get(cur, [])):
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


def _cluster_face_regions(verts, tris, face_bool, with_indices=False, inv=None):
    """Cluster a per-face boolean defect mask into connected regions.

    Faces are joined when they share an edge (``topology.edge_table``'s
    half-edge array is block-stacked: [all (v0,v1), then (v1,v2), then
    (v2,v0)], so half-edge ``h`` belongs to face ``h % F`` -- ``np.tile``, NOT
    ``np.repeat``). Returns the legacy ``detect_non_manifold`` region shape:
    ``{centroid, faces[, verts_idx, faces_idx]}``, canonically ordered by each
    region's sorted vertex ids so the output is permutation-invariant.
    ``inv`` is the (optional) ``topology.edge_table`` half-edge map, passed in
    by a caller that already built it to avoid a second table build."""
    tris = np.asarray(tris, dtype=np.int64)
    F = len(tris)
    face_bool = np.asarray(face_bool, dtype=bool)
    nm_faces = np.nonzero(face_bool)[0].tolist()
    if F == 0 or not nm_faces:
        return []
    if inv is None:
        _edges, _counts, inv = topology.edge_table(tris)
    face_of = np.tile(np.arange(F), 3)
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
    # regions merge; only masked faces can be unioned, so restrict to them
    sel = np.nonzero(np.tile(face_bool, 3))[0]
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
    # ``groups`` insertion order follows face index, so iterating
    # ``groups.values()`` directly would make the region LIST ORDER depend on
    # the input face order.  Sort by the (permutation-invariant) sorted vertex
    # ids of each region so the output is canonical; the per-region content is
    # already order-independent.
    regions = []
    for g in groups.values():
        region_verts = np.unique(tris[g].reshape(-1))
        centroid = np.asarray(verts)[region_verts].mean(axis=0).tolist()
        region = {'centroid': centroid, 'faces': len(g)}
        if with_indices:
            region['verts_idx'] = [int(i) for i in region_verts]
            region['faces_idx'] = [int(i) for i in g]
        regions.append((region_verts, region))
    regions.sort(key=lambda kv: kv[0].tolist())
    return [region for _, region in regions]


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
    # faces adjacent to any non-manifold edge (block-stacked half-edge layout).
    face_of = np.tile(np.arange(F), 3)
    nm_faces = np.unique(face_of[counts[inv] > 2]).tolist()
    face_bool = np.zeros(F, dtype=bool)
    face_bool[nm_faces] = True
    return _cluster_face_regions(verts, tris, face_bool,
                                 with_indices=with_indices, inv=inv)


def detect(verts, tris, with_indices=False):
    """Full defect report: {'holes': [...], 'non_manifold': [...]}.

    ``with_indices`` is passed through to the per-defect detectors; the CLI
    (which calls this without the flag) keeps its JSON contract unchanged,
    while the GUI may request the extra index lists for rendering."""
    return {'holes': detect_holes(verts, tris, with_indices=with_indices),
            'non_manifold': detect_non_manifold(verts, tris,
                                                with_indices=with_indices)}


# Shared per-face defect-type palette: the SINGLE source of truth for both the
# legacy analytic classifier (``defect_type_colors``) and the diagnosis_v2 mask
# map (``defect_type_colors_v2``), so the two can never drift. The three
# historical kinds keep their exact colours; the remaining kinds are
# diagnosis_v2-only overlays. Consumed by the GUI's color-coded mesh view
# (FAZ11). A face keeps its most severe class; (0,0,0) means no defect colour.
DEFECT_COLORS = {
    'degenerate':        (250, 210, 60),   # yellow : zero-area / repeated index
    'non_manifold':      (235, 60, 70),    # red    : non-manifold edge/vertex
    'flipped':           (255, 140, 60),   # orange : flipped (inverted) winding
    'self_intersection': (200, 60, 200),   # magenta: self-intersecting faces
    'duplicate':         (40, 180, 200),   # cyan   : duplicate faces
    'noise':             (140, 140, 140),  # grey   : noise / micro faces
    'interior':          (110, 80, 200),   # indigo : interior faces
    'thin_wall':         (245, 120, 180),  # pink   : thin-wall faces
}

# Historical aliases (the legacy classifier body reads these).
_COLOR_DEGENERATE = DEFECT_COLORS['degenerate']
_COLOR_NON_MANIFOLD = DEFECT_COLORS['non_manifold']
_COLOR_FLIPPED = DEFECT_COLORS['flipped']

# diagnosis_v2 mask bits (rust/sutura-geom/src/diagnosis/mod.rs, thin.rs).
# NOTE: face bit 2 marks faces touching a non-manifold VERTEX only; a classic
# non-manifold EDGE (3+ faces on one edge) is reported by v2 as
# orientation/loops, so the legacy edge detector stays authoritative for it.
_V2_FACE_BITS = {'non_manifold': 2, 'duplicate': 4, 'degenerate': 8,
                 'self_intersection': 16, 'noise': 32, 'interior': 64,
                 'thin_wall': 128, 'orientation': 1}
_V2_VERTEX_BITS = {'non_manifold': 2, 'duplicate': 4, 'loops': 32, 'noise': 64}


def _decode_rle(rle, length):
    """Decode a diagnosis_v2 RLE mask dict into a ``(length,)`` uint8 array.

    The mask arrives as ``{encoding:'rle', length:N, runs:[(start,len,byte),…],
    omitted:bool}``. A missing/omitted mask, an unknown encoding or a malformed
    run decodes to all-zero (never raises)."""
    out = np.zeros(int(length), dtype=np.uint8)
    if not rle or rle.get('omitted') or rle.get('encoding') != 'rle':
        return out
    for run in (rle.get('runs') or []):
        try:
            start, count, value = int(run[0]), int(run[1]), int(run[2])
        except (TypeError, IndexError, ValueError):
            continue
        if count <= 0 or start < 0:
            continue
        out[start:start + count] = value & 0xFF
    return out


def defect_masks_v2(verts, tris):
    """Per-bit defect masks from ``sutura_geom.diagnose`` (render-only).

    Returns ``{'face': {kind: bool_array}, 'vertex': {kind: bool_array}}``
    using the kind names in ``_V2_FACE_BITS``/``_V2_VERTEX_BITS``, or ``None``
    when ``sutura_geom`` is unavailable, ``diagnose`` raises, or either mask
    was omitted (too large). Never raises; the caller falls back to the legacy
    detectors on ``None``."""
    if len(tris) == 0 or len(verts) == 0:
        return None
    try:
        import sutura_geom
    except Exception:  # noqa: BLE001 - render paths must never crash on this
        return None
    if not hasattr(sutura_geom, 'diagnose'):
        return None
    try:
        diag = sutura_geom.diagnose(np.asarray(verts, dtype=np.float32),
                                    np.asarray(tris, dtype=np.int32),
                                    detail='fast', masks=True)
    except Exception:  # noqa: BLE001
        return None
    if not isinstance(diag, dict):
        return None
    fm, vm = diag.get('face_mask'), diag.get('vertex_mask')
    if not fm or not vm or fm.get('omitted') or vm.get('omitted'):
        return None
    face = _decode_rle(fm, len(tris))
    vertex = _decode_rle(vm, len(verts))
    return {'face': {k: (face & b) != 0 for k, b in _V2_FACE_BITS.items()},
            'vertex': {k: (vertex & b) != 0 for k, b in _V2_VERTEX_BITS.items()}}


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
    # The neighbour normals contributing to a face are a permutation-invariant
    # multiset; only their float summation order followed the input face order
    # (``edge_faces.values()``).  ``math.fsum`` is the correctly-rounded exact
    # sum, so ``adj_sum`` -- and therefore the near-threshold ``cosang`` verdict
    # -- no longer depends on that order.
    contrib = [[] for _ in range(F)]
    for faces in edge_faces.values():
        for f1 in faces:
            for f2 in faces:
                if f1 != f2:
                    contrib[f1].append(n[f2])
    adj_sum = np.zeros((F, 3))
    adj_cnt = np.zeros(F)
    for f1 in range(F):
        col = contrib[f1]
        if col:
            adj_sum[f1, 0] = math.fsum(c[0] for c in col)
            adj_sum[f1, 1] = math.fsum(c[1] for c in col)
            adj_sum[f1, 2] = math.fsum(c[2] for c in col)
            adj_cnt[f1] = len(col)
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


# --- diagnosis_v2 render bridge (additive; legacy fallback) -----------------
#
# The render entry points (heatmap_render.py / before_after_render.py /
# viewer_data_render.py) prefer the sutura_geom.diagnose() masks because they
# see defects the legacy analytic detectors cannot (non-manifold VERTICES,
# self-intersections, duplicate/noise/interior faces, thin walls). They are
# strictly ADDITIVE though: the legacy detectors stay authoritative for hole
# diameter markers and non-manifold EDGE markers (v2 has neither), so a pure-v2
# swap would regress the classic red edge overlay. Both helpers below fall back
# to the legacy result when v2 is unavailable.


def defect_map_v2(verts, tris, with_indices=True, masks=None):
    """Defect report for the render paths: legacy markers UNIONED with v2.

    Starts from the legacy ``detect`` report (authoritative for hole diameter
    and non-manifold edge markers) and ADDS the diagnosis_v2 non-manifold
    vertex regions the edge-based legacy detector cannot see. Falls back to
    plain ``detect`` when v2 is unavailable. Requires ``with_indices=True``
    (the union needs per-face indices)."""
    tris = np.asarray(tris, dtype=np.int64)
    base = detect(verts, tris, with_indices=with_indices)
    if not with_indices:
        return base
    if masks is None:
        masks = defect_masks_v2(verts, tris)
    if masks is None:
        return base
    nm_face = np.zeros(len(tris), dtype=bool)
    for d in (base.get('non_manifold') or []):
        fi = d.get('faces_idx')
        if fi:
            nm_face[np.asarray(fi, dtype=np.int64)] = True
    v2_nm_face = np.asarray(masks['face'].get('non_manifold'), dtype=bool)
    if v2_nm_face.size == len(tris):
        nm_face |= v2_nm_face
    nm_vertex = np.asarray(masks['vertex'].get('non_manifold'), dtype=bool)
    if nm_vertex.size == len(verts) and nm_vertex.any():
        nm_face |= nm_vertex[tris].any(axis=1)
    return {'holes': base.get('holes') or [],
            'non_manifold': _cluster_face_regions(verts, tris, nm_face,
                                                  with_indices=True)}


def defect_type_colors_v2(verts, tris, masks=None):
    """Per-face defect-type colour array (M,3) uint8, v2-enhanced.

    Same contract as ``defect_type_colors`` (render-only; ``(0,0,0)`` means
    "no defect colour"). Starts from the legacy colour map (authoritative for
    the shared kinds -- degenerate / non-manifold edge / flipped) and ADDS the
    diagnosis_v2-only kinds (non-manifold vertices, self-intersections,
    duplicate / noise / interior / thin-wall faces) for faces the legacy map
    left uncoloured, using the shared ``DEFECT_COLORS`` palette. Falls back to
    plain ``defect_type_colors`` when v2 is unavailable. More severe kinds win:
    non-manifold > self-intersection > duplicate > noise > interior > thin-wall."""
    colors = defect_type_colors(verts, tris)
    if masks is None:
        masks = defect_masks_v2(verts, tris)
    if masks is None:
        return colors
    t = np.asarray(tris, dtype=np.int64)
    F = len(t)
    if F == 0 or len(colors) != F:
        return colors
    face, vertex = masks['face'], masks['vertex']
    uncolored = ~colors.any(axis=1)

    def _bit(src, key):
        m = np.asarray(src.get(key), dtype=bool)
        return m if m.size == F else None

    nmv = np.asarray(vertex.get('non_manifold'), dtype=bool)
    nmv_face = (nmv[t].any(axis=1) if nmv.size == len(verts)
                else np.zeros(F, dtype=bool))
    priority = [('non_manifold', nmv_face),
                ('self_intersection', _bit(face, 'self_intersection')),
                ('duplicate', _bit(face, 'duplicate')),
                ('noise', _bit(face, 'noise')),
                ('interior', _bit(face, 'interior')),
                ('thin_wall', _bit(face, 'thin_wall'))]
    for name, m in priority:
        if m is None:
            continue
        sel = m & uncolored
        if sel.any():
            colors[sel] = DEFECT_COLORS[name]
        uncolored = uncolored & ~m
    return colors
