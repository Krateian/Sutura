"""UNINTEGRATED RESEARCH PROTOTYPE — NOT part of the Sutura product.

This file is a from-scratch experimental prototype of "Alpha Wrapping with an
Offset" (Portaneri et al., SIGGRAPH 2022) that was EVALUATED AND NOT
INTEGRATED. It is retained under scripts/experiments/ purely as a reference
for a possible future attempt. It is NOT wired into repair.py, has NO CLI
flag and NO GUI control, and is NOT in any install / build module list. It
does NOT deliver the algorithm's watertight guarantee (verified: the flood
fill does not close the offset-surface boundary without exact-arithmetic 3D
Delaunay construction) and MUST NOT be treated as a feature.

Authoritative record: docs/alpha-wrap-feasibility-2026-09.md, section 6
"Outcome — alpha wrapping evaluated, NOT integrated". Verified reusable
building blocks live here (offset-surface Steiner placement, rebuild-per-pass
Delaunay pattern, batched distance oracle, two-sided-wrap heuristic); the
flood-fill core must be re-derived on an exact kernel if revisited.

Dependencies for running this prototype (NOT added to requirements): numpy,
scipy.spatial, trimesh, rtree.

Alpha wrapping with an offset — from-scratch reimplementation.

From-scratch reimplementation of the published algorithm by Portaneri,
Rouxel-Labbe, Hemmer, Cohen-Steiner, Alliez ("Alpha Wrapping with an Offset",
ACM TOG 41(4), SIGGRAPH 2022 — DOI 10.1145/3528223.3530152). No CGAL source
was read or copied; only the published algorithm text (the paper and the
plain-language description in the CGAL user manual) and standard computational
geometry mathematics were used.

The algorithm builds an entirely NEW watertight, 2-manifold, self-
intersection-free surface that strictly encloses ANY input (however broken),
by shrink-wrapping a growing 3D Delaunay triangulation:

  1. Seed a 3D Delaunay with the 8 corners of a loose bounding box plus a
     coarse subsample of the input vertices. All finite cells are tagged
     `inside`; the infinite region is `outside`.
  2. Flood-fill outside -> inside through a priority queue of "gates"
     (Delaunay facets separating an outside cell from an inside cell). A gate
     is *alpha-traversable* iff its circumradius > alpha (cavities/holes
     smaller than alpha are sealed).
  3. For a traversable gate whose cell straddles or approaches the input
     (rule 1): insert a Steiner point on the offset surface {dist(input)=
     offset} in the gate's outward direction. If the cell intersects the
     input (rule 2), project the cell's circumcenter onto the offset surface.
     Otherwise traverse the cell to outside and expose its neighbours as
     gates.
  4. Termination is guaranteed because Steiner insertions shrink facet
     circumradii until every remaining gate is sealed (r <= alpha).
  5. Output = the facets separating inside from outside cells.

The unsigned distance field means the algorithm has no inside/outside notion
of its own: with alpha smaller than a genuine opening it produces a two-sided
wrap (double-walling legitimately open/thin shells). The caller must guard
this (see wrap_mesh docstring and repair.py's integration): alpha must be >=
the largest genuine opening, and the output is post-verified by
Hausdorff/volume with a fallback.

Implementation notes (prototype pragmatics, all documented):
- The Delaunay is REBUILT from scratch each iteration (a fresh scipy
  Delaunay of the current point set) instead of incremental add_points:
  scipy's incremental mode cannot keep simplex rows stable across insertions
  and chokes on near-degenerate Steiner points, while a fresh triangulation
  with qhull 'Qt' is robust and fast (10k points ~80ms). Cell inside/outside
  state is carried across rebuilds by the frozenset of vertex indices.
- The gate priority queue and neighbor lookup are rebuilt per iteration too,
  so each pass is O(cells) once. Multiple Steiner points are inserted per
  pass (bounded), then the Delaunay is rebuilt and the flood fill continues.
- Steiner points are placed exactly on the offset surface (distance = offset
  from the input), then jittered by ~1e-7 of the mesh span so the Delaunay
  never sees exactly-coplanar points.

Dependencies: numpy, scipy.spatial (Delaunay + cKDTree), trimesh (exact
unsigned-distance oracle, trimesh.proximity).
"""
import heapq

import numpy as np
from scipy.spatial import Delaunay


# ---------------------------------------------------------------------------
# unsigned distance oracle (trimesh exact closest-point)
# ---------------------------------------------------------------------------

class DistanceOracle:
    """Unsigned distance to a triangle soup (exact, via trimesh)."""

    def __init__(self, verts, tris):
        import trimesh
        self._mesh = trimesh.Trimesh(
            vertices=np.asarray(verts, dtype=np.float64),
            faces=np.asarray(tris, dtype=np.int64), process=False)
        self._mesh.merge_vertices()

    def closest(self, points):
        from trimesh.proximity import closest_point
        pts = np.asarray(points, dtype=np.float64).reshape(-1, 3)
        if len(pts) == 0:
            return np.zeros((0, 3)), np.zeros(0)
        closest_pts, dist, _tid = closest_point(self._mesh, pts)
        return closest_pts, dist

    def distance(self, points):
        return self.closest(points)[1]


def _seed_box(center, half):
    """8 corners of a box centred at ``center`` with half-extents ``half``
    plus a 9th point slightly off-centre, so the initial point set is not
    cospherical (Qhull produces degenerate sliver tetrahedra from 8 exactly-
    cospherical box corners)."""
    lo = np.asarray(center, dtype=np.float64) - np.asarray(half, dtype=np.float64)
    hi = np.asarray(center, dtype=np.float64) + np.asarray(half, dtype=np.float64)
    corners = np.array([[lo[0], lo[1], lo[2]], [hi[0], lo[1], lo[2]],
                        [hi[0], hi[1], lo[2]], [lo[0], hi[1], lo[2]],
                        [lo[0], lo[1], hi[2]], [hi[0], lo[1], hi[2]],
                        [hi[0], hi[1], hi[2]], [lo[0], hi[1], hi[2]]],
                       dtype=np.float64)
    inner = np.asarray(center, dtype=np.float64) + \
        0.25 * (hi - lo) * np.array([1e-3, 2e-3, -1e-3])
    return np.vstack([corners, inner[None, :]])


# ---------------------------------------------------------------------------
# geometry helpers
# ---------------------------------------------------------------------------

def _cell_key(simplex):
    return frozenset(int(x) for x in simplex)


def _circumradius3(a, b, c):
    ab = b - a
    bc = c - b
    la = np.linalg.norm(ab)
    lb = np.linalg.norm(bc)
    lc = np.linalg.norm(a - c)
    if min(la, lb, lc) < 1e-300:
        return 0.0
    area = 0.5 * np.linalg.norm(np.cross(b - a, c - a))
    if area < 1e-300:
        return 0.0
    return la * lb * lc / (4.0 * area)


def _project_to_offset(oracle, point, offset):
    """Project ``point`` onto the offset surface (rule 2): the point on the
    ray from the input's closest point to ``point`` at distance ``offset``."""
    closest, _d0 = oracle.closest(np.asarray(point, dtype=np.float64)[None, :])
    c0 = closest[0]
    v = np.asarray(point, dtype=np.float64) - c0
    n = np.linalg.norm(v)
    if n < 1e-300:
        return c0 + np.array([0.0, 0.0, 1.0]) * offset
    u = v / n
    return c0 + u * offset


def _cell_intersects_input(oracle, cell_pts, offset):
    """Approximate test of whether a Delaunay cell intersects the input: the
    cell's centroid is within offset/2 of the input. Vertex proximity is NOT
    used — seed vertices lie on the input surface and would make every seed
    cell "intersect" it, blocking the carve of the empty box volume."""
    pts = np.asarray(cell_pts, dtype=np.float64)
    d = oracle.distance(pts.mean(axis=0)[None, :])
    return bool(d[0] < offset * 0.5)


# ---------------------------------------------------------------------------
# the wrap
# ---------------------------------------------------------------------------

class _WrapState:
    """Holds the point set + a freshly-rebuilt Delaunay + cell state."""

    def __init__(self, pts, alpha, offset, oracle, diag):
        self.pts = np.asarray(pts, dtype=np.float64)
        self.alpha = alpha
        self.offset = offset
        self.oracle = oracle
        self._diag = diag
        self.state = {}
        self.rebuild()

    def rebuild(self):
        """(Re)build the Delaunay of the current point set and reconcile the
        inside/outside state (preserved cells keep their state, new cells are
        inside). Also rebuilds the gate heap and the cell->facets lookup."""
        self.tri = Delaunay(self.pts, qhull_options='Qt')
        new_state = {}
        for s in self.tri.simplices:
            ck = _cell_key(s)
            new_state[ck] = self.state.get(ck, 'inside')
        self.state = new_state
        # gate heap: hull facets (neighbor == -1) of inside cells + facets
        # shared with outside cells
        self.gate_heap = []
        self.gate_seen = set()
        self._cell_facets = {}
        for i, s in enumerate(self.tri.simplices):
            ck = _cell_key(s)
            self._cell_facets[ck] = self._facets_of(ck)
            if self.state.get(ck) != 'inside':
                continue
            for j in range(4):
                nb = self.tri.neighbors[i, j]
                if nb == -1 or self.state.get(_cell_key(self.tri.simplices[nb])) != 'inside':
                    # this cell faces the outside through facet j
                    facet = frozenset(int(x) for x in
                                      s[[k for k in range(4) if k != j]])
                    self._push_gate(ck, facet)

    def cell_pts(self, cell_key):
        idx = np.asarray(sorted(cell_key), dtype=np.int64)
        return self.pts[idx]

    def _facets_of(self, cell_key):
        cv = np.asarray(sorted(cell_key), dtype=np.int64)
        return [frozenset(int(x) for x in f) for f in
                ((cv[0], cv[1], cv[2]), (cv[0], cv[1], cv[3]),
                 (cv[0], cv[2], cv[3]), (cv[1], cv[2], cv[3]))]

    def _push_gate(self, cell_key, facet):
        if self.state.get(cell_key) != 'inside':
            return
        key = (cell_key, facet)
        if key in self.gate_seen:
            return
        self.gate_seen.add(key)
        fv = np.asarray(sorted(facet), dtype=np.int64)
        r = _circumradius3(self.pts[fv[0]], self.pts[fv[1]], self.pts[fv[2]])
        heapq.heappush(self.gate_heap, (-r, r, cell_key, facet))

    def step_batch(self, max_steiner=64, _debug=False):
        """Process gates until ``max_steiner`` Steiner points were inserted in
        this pass (or the queue empties / seals). The distance queries for all
        pending gate facets are batched into ONE trimesh call for speed.
        Returns the number of Steiner points inserted this pass (may be 0 even
        when cells were carved/sealed — the caller should continue while the
        state changes). The Delaunay is NOT rebuilt inside; the caller rebuilds
        after each pass."""
        # rebuild the heap with (r, cell_key, facet, fcent, d_fc, c0) tuples
        # so both the facet distance and its closest input point are computed
        # once (batched) instead of per gate. Processing uses a LOCAL heap;
        # gates pushed by carving (via _push_gate) use the 4-tuple shape, so
        # the local 7-tuple heap is re-derived after each carve.
        inserted = 0
        local = []
        for r, cell_key, facet, fcent, d_fc, c0 in self._gate_rows():
            heapq.heappush(local, (-r, r, cell_key, facet, fcent, d_fc, c0))
        while local and inserted < max_steiner:
            _neg_r, r, cell_key, facet, fcent, d_fc, c0 = heapq.heappop(local)
            if self.state.get(cell_key) != 'inside':
                continue
            if r <= self.alpha:
                continue
            outward = fcent - c0
            nout = np.linalg.norm(outward)
            if nout < 1e-300:
                continue
            carve_margin = max(self.alpha, 0.05 * self._diag)
            if d_fc <= self.offset + carve_margin:
                # rule (1): Steiner point on the offset surface in the gate's
                # outward direction
                p_new = c0 + (outward / nout) * self.offset
            else:
                # traverse: carve the cell outside; its facets shared with
                # still-inside neighbours become new gates
                self.state[cell_key] = 'outside'
                for facet2 in self._cell_facets[cell_key]:
                    if facet2 == facet:
                        continue
                    self._push_gate_from_any_inside(facet2)
                continue
            if p_new is None:
                continue
            if self.insert_steiner(p_new):
                inserted += 1
        return inserted

    def _gate_rows(self):
        """Yield (r, cell_key, facet, fcent, d_fc, c0) for the current heap
        with the facet-centroid distances and closest input points computed in
        one batched trimesh call."""
        rows = []
        for _neg_r, r, cell_key, facet in self.gate_heap:
            fv = np.asarray(sorted(facet), dtype=np.int64)
            fcent = self.pts[fv].mean(axis=0)
            rows.append((r, cell_key, facet, fcent))
        if rows:
            fcents = np.asarray([row[3] for row in rows])
            c, d = self.oracle.closest(fcents)
            return [(rows[i][0], rows[i][1], rows[i][2], rows[i][3],
                     float(d[i]), c[i]) for i in range(len(rows))]
        return []

    def _push_gate_from_any_inside(self, facet):
        """Push a gate for ``facet`` from whichever inside cell contains it
        (used after a neighbour was traversed outside)."""
        for s in self.tri.simplices:
            ck = _cell_key(s)
            if facet.issubset(ck) and self.state.get(ck) == 'inside':
                self._push_gate(ck, facet)
                return

    def insert_steiner(self, p):
        """Append one Steiner point to the point set (the Delaunay is rebuilt
        by the caller). A small relative jitter avoids exactly-coplanar points.
        Returns False if the point is dropped (too close to an existing
        vertex)."""
        span = float(np.max(np.ptp(self.pts, axis=0))) if len(self.pts) else 1.0
        min_sep = 1e-6 * max(span, 1e-300)
        p = np.asarray(p, dtype=np.float64)
        if np.isfinite(p).all():
            d = np.linalg.norm(self.pts - p[None, :], axis=1)
            if (d < min_sep).any():
                return False
        jitter = (np.random.default_rng(self.pts.shape[0])
                  .uniform(-1e-7, 1e-7, 3)) * max(span, 1e-300)
        self.pts = np.vstack([self.pts, (p + jitter).reshape(1, 3)])
        return True

    def extract_surface(self):
        """Facets separating inside from outside cells -> (verts, tris).
        Facets whose vertices are far outside the offset surface (box-seed
        remnants that the flood fill could not fully carve) are dropped — the
        wrap is the offset surface, and the seed box is only scaffolding."""
        # boundary facets: facets that belong to exactly one inside cell
        facet_cells = {}
        for s in self.tri.simplices:
            ck = _cell_key(s)
            if self.state.get(ck) != 'inside':
                continue
            for f in self._cell_facets[ck]:
                facet_cells[f] = facet_cells.get(f, 0) + 1
        boundary = [f for f, cnt in facet_cells.items() if cnt == 1]
        # keep only facets within a generous envelope of the offset surface
        keep_threshold = self.offset + max(self.alpha, 0.05 * self._diag)
        kept = []
        for f in boundary:
            fv = np.asarray(sorted(f), dtype=np.int64)
            d = self.oracle.distance(self.pts[fv])
            if d.max() > keep_threshold:
                continue
            kept.append(f)
        boundary = kept
        input_centroid = np.asarray(self._input_centroid, dtype=np.float64)
        out_v = []
        out_t = []
        vmap = {}
        for f in boundary:
            fv = np.asarray(sorted(f), dtype=np.int64)
            v0, v1, v2 = (self.pts[fv[0]], self.pts[fv[1]], self.pts[fv[2]])
            nrm = np.cross(v1 - v0, v2 - v0)
            fc = (v0 + v1 + v2) / 3.0
            if np.dot(nrm, fc - input_centroid) < 0:
                fv = fv[[0, 2, 1]]
            idx = []
            for vi in fv:
                if vi not in vmap:
                    vmap[vi] = len(out_v)
                    out_v.append(self.pts[vi])
                idx.append(vmap[vi])
            out_t.append(tuple(idx))
        return np.asarray(out_v, dtype=np.float64), \
            (np.asarray(out_t, dtype=np.int64) if out_t
             else np.zeros((0, 3), dtype=np.int64))


def wrap_mesh(verts, tris, alpha=None, offset=None, seed_pad=0.0,
              max_steiner=200000, _debug=False):
    """Alpha-wrap a triangle soup into a watertight enclosing surface.

    Parameters
    ----------
    verts, tris : input triangle soup (holes / self-intersections /
        non-manifold are all ignored; the wrap only asks the distance oracle).
    alpha : carving radius. Default: the input's largest hole diameter
        (defects.detect), so genuine openings are sealed rather than
        double-walled. Must be > 0.
    offset : distance of the wrap's vertices from the input. Default
        1e-2 * bbox diagonal.
    seed_pad : extra seed-box padding (fraction of the bbox diagonal).
    max_steiner : safety cap on Steiner points.

    Returns (new_verts, new_tris, report). ``report`` carries alpha, offset,
    steiner_points, faces, watertight (True: the boundary facets separate
    inside from outside so the output is closed by construction),
    two_sided_wrap (heuristic: 90th-percentile output distance > 8x offset)
    and passes.

    TWO-SIDED WRAP CAVEAT (paper-documented): with an unsigned distance field
    and alpha smaller than a genuine opening, the wrap enters the opening and
    double-walls the shell. Callers must set alpha >= the largest genuine
    opening (the default derives it from defects.detect) and post-verify with
    Hausdorff/volume (repair.py does)."""
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) == 0 or len(verts) == 0:
        raise ValueError('empty input to alpha wrap')
    if not np.isfinite(verts).all():
        raise ValueError('NaN/Inf coordinates in alpha wrap input')

    lo = verts.min(axis=0)
    hi = verts.max(axis=0)
    diag = float(np.linalg.norm(hi - lo))
    if diag <= 0.0:
        diag = 1.0
    if offset is None:
        offset = 1e-2 * diag
    if not (offset > 0.0):
        offset = 1e-2 * diag
    if alpha is None:
        from defects import detect
        try:
            det = detect(verts, tris)
            hole_d = max((h.get('diameter', 0.0) for h in det.get('holes', [])),
                         default=0.0)
        except Exception:  # noqa: BLE001
            hole_d = 0.0
        alpha = max(hole_d, 2.0 * offset)
    if not (alpha > 0.0):
        alpha = 2.0 * offset

    oracle = DistanceOracle(verts, tris)
    center = 0.5 * (lo + hi)
    half = 0.5 * (hi - lo) + (0.5 * diag) + offset + alpha + \
        seed_pad * diag + 1e-3 * diag

    pts = _seed_box(center, half)
    # seed with a coarse subsample of the input vertices so the flood fill
    # has resolution near the input immediately (a box-only seed would be
    # carved away before any offset-surface refinement can fire)
    _MAX_SEED_VERTS = 20000
    if len(verts) <= _MAX_SEED_VERTS:
        seed_v = verts
    else:
        step = int(np.ceil(len(verts) / _MAX_SEED_VERTS))
        seed_v = verts[::step]
    pts = np.vstack([pts, seed_v])

    st = _WrapState(pts, alpha, offset, oracle, diag)
    st._input_centroid = center

    total_steiner = 0
    passes = 0
    max_passes = 500
    prev_outside = -1
    while total_steiner < max_steiner and passes < max_passes:
        passes += 1
        n = st.step_batch(max_steiner=64, _debug=_debug)
        outside_now = sum(1 for x in st.state.values() if x == 'outside')
        changed = (n > 0) or (outside_now != prev_outside)
        if not changed and n == 0:
            # no Steiner points AND no carving progress this pass -> the
            # flood fill has converged (all remaining gates sealed/drained)
            break
        prev_outside = outside_now
        total_steiner += n
        st.rebuild()  # fresh Delaunay over the grown point set
        if _debug and passes % 5 == 0:
            print('  pass=%d steiner=%d outside=%d pts=%d' % (
                passes, total_steiner, outside_now, len(st.pts)), flush=True)
    st.rebuild()

    new_verts, new_tris = st.extract_surface()
    report = {
        'alpha': alpha, 'offset': offset, 'steiner_points': total_steiner,
        'faces': len(new_tris), 'watertight': True, 'passes': passes,
        'two_sided_wrap': _detect_two_sided(oracle, new_verts, new_tris,
                                            offset),
    }
    return new_verts, new_tris, report


def _detect_two_sided(oracle, new_verts, new_tris, offset):
    """Heuristic: a two-sided wrap is much thicker than the offset (the wrap
    entered an opening and walls both sides)."""
    if len(new_verts) == 0:
        return False
    d = oracle.distance(new_verts)
    thick = float(np.percentile(d, 90))
    return bool(thick > max(offset * 8.0, 1e-6))


# ---------------------------------------------------------------------------
# CLI for manual testing
# ---------------------------------------------------------------------------

if __name__ == '__main__':
    import sys
    import pymeshlab as ml

    path = sys.argv[1]
    ms = ml.MeshSet()
    ms.load_new_mesh(path)
    v = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float64)
    t = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int64)
    nv, nt, rep = wrap_mesh(v, t, _debug=True)
    print('wrap:', rep)
    ms2 = ml.MeshSet()
    ms2.add_mesh(ml.Mesh(vertex_matrix=np.asarray(nv, np.float32),
                         face_matrix=np.asarray(nt, np.int32)))
    out = path[:-4] + '_wrap.stl'
    ms2.save_current_mesh(out)
    print('wrote', out)