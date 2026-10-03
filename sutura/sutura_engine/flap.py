"""Flap: surface-based hole filling (Liepa-style surgical flap).

A surgical "flap" covers a defect and continues the neighbouring tissue: every
boundary loop is triangulated with a minimum-area dynamic program, refined to
the surrounding edge length, and faired with a discrete biharmonic
(thin-plate) solve whose boundary condition carries the position AND
slope/curvature of the surrounding original surface (a C1 ghost ring built
from each rim vertex's original 1-ring step).

No voxels are used and the original triangles are kept verbatim: every output
face is either an original input face or a flap face built from the hole's own
boundary vertices plus refined interior points.

Pure numpy/scipy; no pymeshlab, trimesh or I/O.  The public entry point is
:func:`flap_fill`; it is wired into the Stage-1 chain behind the off-by-default
Flap switch.
"""
from __future__ import annotations

import numpy as np
import scipy.sparse as sp
import scipy.sparse.csgraph as csgraph
import scipy.sparse.linalg as spla
from scipy.spatial import cKDTree


# ---------------------------------------------------------------------------
# Rust core selection
# ---------------------------------------------------------------------------
# ``sutura_geom.flap_fill`` is the Rust port of this module's oracle (bit-for-
# bit identical on the validated corpus, and ~6x faster on framebaroque).  It is
# feature-detected here, exactly like the other Rust paths (``sdf_grid`` /
# ``dressing_coat`` in ``sutura_engine.dressing``): a build without it, or one
# whose extension predates the binding, transparently falls back to the numpy
# implementation below.  ``None`` means "not probed yet"; ``False`` means
# "probed and absent".
_RUST_FLAP = None


def _load_rust_flap():
    """Return ``sutura_geom.flap_fill`` when the extension provides it, else None.

    The result is cached. The lookup never raises: an absent/incompatible
    extension degrades to the numpy oracle.
    """
    global _RUST_FLAP
    if _RUST_FLAP is None:
        try:
            import sutura_geom as _sg
            fn = getattr(_sg, 'flap_fill', None)
            _RUST_FLAP = fn if callable(fn) else False
        except Exception:  # noqa: BLE001 - absence is not an error
            _RUST_FLAP = False
    return _RUST_FLAP or None


def _face_cent_norm(P, faces):
    """Centroid, unit normal and longest-edge length of each triangle."""
    tri = P[np.asarray(faces, np.int64)]
    c = tri.mean(axis=1)
    n = np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0])
    ln = np.linalg.norm(n, axis=1)
    ln[ln == 0] = 1.0
    n = n / ln[:, None]
    ml = np.max(np.linalg.norm(tri - tri[:, [1, 2, 0], :], axis=2), axis=1)
    return c, n, ml


def _component_stats(v2, t2, f_rfc, f_ffo, f_frs, angle_deg=2.0):
    """Per-face component labels (shared-edge adjacency) plus signed volume,
    surface area and a flat/coplanar flag per component.

    Used to refuse a fill that would close a component into a zero-volume
    double-face shell (agy: the 25 spurious framebaroque parts).  A component
    whose signed volume after the fill is ~0 for its surface area is a flat
    sheet, not a real solid.
    """
    F = len(t2)
    tri = v2[t2]
    n = np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0])
    ln = np.linalg.norm(n, axis=1)
    a2 = 0.5 * ln
    n = n / np.maximum(ln[:, None], 1e-30)
    vol_f = np.einsum("ij,ij->i", tri[:, 0],
                      np.cross(tri[:, 1], tri[:, 2])) / 6.0
    if F == 0:
        return (np.zeros(0, np.int64), np.zeros(0), np.zeros(0),
                np.zeros(0, bool))
    two = f_rfc == 2
    fa = f_ffo[f_frs[two]]
    fb = f_ffo[f_frs[two] + 1]
    g = sp.coo_matrix((np.ones(len(fa)), (fa, fb)), shape=(F, F))
    _, labels = csgraph.connected_components(g, directed=False)
    nu = int(labels.max()) + 1
    vol_c = np.zeros(nu)
    np.add.at(vol_c, labels, vol_f)
    area_c = np.zeros(nu)
    np.add.at(area_c, labels, a2)
    mn = np.zeros((nu, 3))
    np.add.at(mn, labels, n * a2[:, None])
    mn /= np.maximum(np.linalg.norm(mn, axis=1, keepdims=True), 1e-30)
    dot = np.einsum("ij,ij->i", n, mn[labels])
    comp_min = np.ones(nu)
    np.minimum.at(comp_min, labels, dot)
    flat_c = comp_min > np.cos(np.deg2rad(angle_deg))
    return labels, vol_c, area_c, flat_c


def _overlap_area_2d(P2, Q2):
    """Intersection area of two 2-D polygons (Sutherland-Hodgman)."""
    def clip(poly, a, b):
        out = []
        nx, ny = len(poly), len(poly)
        for i in range(nx):
            cur = poly[i]
            nxt = poly[(i + 1) % ny]
            dc = (b[0] - a[0]) * (cur[1] - a[1]) - (b[1] - a[1]) * (cur[0] - a[0])
            dn = (b[0] - a[0]) * (nxt[1] - a[1]) - (b[1] - a[1]) * (nxt[0] - a[0])
            if dc >= -1e-12:
                out.append(cur)
            if (dc > 1e-12 and dn < -1e-12) or (dc < -1e-12 and dn > 1e-12):
                t = dc / (dc - dn)
                out.append((cur[0] + t * (nxt[0] - cur[0]),
                            cur[1] + t * (nxt[1] - cur[1])))
        return out
    if len(P2) < 3 or len(Q2) < 3:
        return 0.0
    poly = [tuple(p) for p in P2]
    for i in range(len(Q2)):
        if not poly:
            return 0.0
        poly = clip(poly, Q2[i], Q2[(i + 1) % len(Q2)])
    if len(poly) < 3:
        return 0.0
    area = 0.0
    for i in range(len(poly)):
        x1, y1 = poly[i]
        x2, y2 = poly[(i + 1) % len(poly)]
        area += x1 * y2 - x2 * y1
    return abs(area) * 0.5


def _tri_area2d(T):
    return 0.5 * abs((T[1, 0] - T[0, 0]) * (T[2, 1] - T[0, 1]) -
                     (T[2, 0] - T[0, 0]) * (T[1, 1] - T[0, 1]))


def _pairs_overlap(tri_a, tri_b, rn_b, chunk=200000, eps_rel=1e-9):
    """Vectorized positive-area overlap test for batches of coplanar triangle
    pairs (separating-axis theorem, projected into each B triangle's plane).

    Two triangles that merely share an edge touch on the axis perpendicular to
    that edge (``max == min``) and are therefore reported as non-overlapping,
    matching the Sutherland-Hodgman area test's zero-area edge case.
    """
    K = len(tri_a)
    out = np.zeros(K, bool)
    for s in range(0, K, chunk):
        e = min(s + chunk, K)
        A3, B3, rn = tri_a[s:e], tri_b[s:e], rn_b[s:e]
        rt0 = B3[:, 0, :]
        u = B3[:, 1, :] - rt0
        lu = np.linalg.norm(u, axis=1)
        ok = lu > 1e-12
        u = u / np.maximum(lu, 1e-30)[:, None]
        w = np.cross(rn, u)
        rel = np.stack([u, w], axis=1)                      # (k,2,3) basis x xyz
        A = np.einsum("kij,kcj->kic", A3 - rt0[:, None, :], rel)
        B = np.einsum("kij,kcj->kic", B3 - rt0[:, None, :], rel)
        edge = np.concatenate([A3[:, 1, :] - A3[:, 0, :],
                               A3[:, 2, :] - A3[:, 1, :],
                               A3[:, 0, :] - A3[:, 2, :],
                               B3[:, 1, :] - B3[:, 0, :],
                               B3[:, 2, :] - B3[:, 1, :],
                               B3[:, 0, :] - B3[:, 2, :]], axis=0)
        scale = np.maximum(np.linalg.norm(edge, axis=1).reshape(6, -1).max(0),
                           1e-12)
        eps = eps_rel * scale
        sep = np.zeros(e - s, bool)
        ov = np.ones(e - s, bool) & ok
        for T in (A, B):
            t0 = T[:, 1, :] - T[:, 0, :]
            t1 = T[:, 2, :] - T[:, 1, :]
            t2 = T[:, 0, :] - T[:, 2, :]
            for te in (t0, t1, t2):
                ax = np.stack([-te[:, 1], te[:, 0]], axis=1)
                pa = np.einsum("kvc,kc->kv", A, ax)
                pb = np.einsum("kvc,kc->kv", B, ax)
                sep |= (pa.max(1) <= pb.min(1) + eps) | \
                       (pb.max(1) <= pa.min(1) + eps)
        out[s:e] = ov & ~sep
    return out


def _coplanar_collides(P, faces, base, extra, overlap_frac=0.05,
                       cos_tol=0.99, plane_frac=1e-3):
    """True when a candidate patch face has a positive-area coplanar overlap
    with a committed face (``base`` = original surface, ``extra`` = patches
    committed so far) or with another face of the same patch.

    Adjacent coplanar triangles that share only an edge overlap in zero area,
    so they are not rejected; a genuine coincident/overlapping patch (the
    double surface that breaks manifold3d's union) is.  ``base``/``extra``
    items are ``(centroid, normal, tris(k,3,3), tree)``.
    """
    faces = np.asarray(faces, np.int64)
    F = len(faces)
    tri = np.asarray(P, np.float64)[faces]
    c, n, ml = _face_cent_norm(P, faces)
    if F == 0:
        return False

    # Gather every reference face (committed patches first, then the candidate
    # patch itself for the intra-patch check) into flat arrays.
    rc_l, rn_l, rt_l, intra_l = [], [], [], []
    for (rc, rn, rtri, _tree) in list(base) + list(extra):
        rc = np.asarray(rc, np.float64)
        if len(rc):
            rc_l.append(rc)
            rn_l.append(np.asarray(rn, np.float64))
            rt_l.append(np.asarray(rtri, np.float64))
            intra_l.append(np.zeros(len(rc), bool))
    off = int(sum(len(x) for x in rc_l))
    # Intra-patch overlap is only meaningful for a genuinely folded patch and
    # is O(F * local-density); on a huge refined fan it is both expensive and
    # redundant (a conforming triangulation is interior-disjoint by
    # construction).  The committed-patch check below always runs.
    do_intra = F <= COPLANAR_INTRA_MAX
    if not do_intra and off == 0:
        return False
    if do_intra:
        rc_l.append(c)
        rn_l.append(n)
        rt_l.append(tri)
        intra_l.append(np.ones(F, bool))
    Rc = np.concatenate(rc_l)
    Rn = np.concatenate(rn_l)
    Rtri = np.concatenate(rt_l)
    Rintra = np.concatenate(intra_l)

    tree = cKDTree(Rc)
    # Per-face radius (2x its own longest edge), capped at a robust scale so a
    # few uncapped giant faces cannot trigger an unbounded neighbour list.
    _cap = 4.0 * float(np.median(ml)) if len(ml) else 0.0
    radius = 2.0 * np.minimum(ml, _cap) if _cap > 0 else np.zeros(len(ml))
    cand = tree.query_ball_point(c, np.maximum(radius, 1e-12), workers=1)
    I, J = [], []
    for i, js in enumerate(cand):
        for j in js:
            if Rintra[j] and j == off + i:
                continue
            I.append(i)
            J.append(j)
    if not I:
        return False
    I = np.asarray(I, np.int64)
    J = np.asarray(J, np.int64)

    keep = np.abs(np.einsum("ij,ij->i", n[I], Rn[J])) >= cos_tol
    if not keep.any():
        return False
    I, J = I[keep], J[keep]
    d = np.einsum("ijk,ik->ij", tri[I] - Rtri[J][:, 0, :][:, None, :], Rn[J])
    keep = np.max(np.abs(d), axis=1) <= plane_frac * np.maximum(ml[I], 1e-9)
    if not keep.any():
        return False
    I, J = I[keep], J[keep]
    return bool(_pairs_overlap(tri[I], Rtri[J], Rn[J]).any())


# ---------------------------------------------------------------------------
# edge / topology helpers
# ---------------------------------------------------------------------------
def _edge_keys(tris, nverts):
    """Undirected edge keys for the 3 edges of each face."""
    e = np.concatenate([tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]], axis=0)
    s = np.sort(e, axis=1)
    return s, s[:, 0].astype(np.int64) * np.int64(nverts) + s[:, 1].astype(np.int64)


def non_manifold_stats(tris, nverts):
    _s, keys = _edge_keys(np.asarray(tris, np.int64), nverts)
    _u, ct = np.unique(keys, return_counts=True)
    return {
        "boundary_edges": int((ct == 1).sum()),
        "non_manifold_edges": int((ct > 2).sum()),
        "faces_on_nm_edges": int(ct[ct > 2].sum()) if (ct > 2).any() else 0,
    }


def split_non_manifold(verts, tris):
    """Cut the mesh along every non-manifold edge and vertex so all remaining
    edges are manifold and the non-manifold fans become boundary loops.

    Standard "vertex-fan explosion": corners (face, local vertex) are joined
    across an edge only when that edge has exactly two incident faces.  Each
    connected fan gets its own copy of the vertex, so a non-manifold edge
    (>2 incident faces) is never traversed (it is split) and a non-manifold
    vertex (two fans meeting at a point) is duplicated.

    Returns (verts2, tris2, info).
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    nv = len(verts)
    if len(tris) == 0:
        return verts, tris, {"splits": 0, "nm_edges_in": 0}
    good = (tris[:, 0] != tris[:, 1]) & (tris[:, 1] != tris[:, 2]) & (tris[:, 2] != tris[:, 0])
    dropped = int((~good).sum())
    tris = tris[good]
    F = len(tris)
    if F == 0:
        return verts, tris, {"splits": 0, "nm_edges_in": 0,
                             "faces_dropped_degenerate": dropped}

    starts = np.concatenate([tris[:, 0], tris[:, 1], tris[:, 2]])
    ends = np.concatenate([tris[:, 1], tris[:, 2], tris[:, 0]])
    face_of = np.tile(np.arange(F, dtype=np.int64), 3)
    zero = np.zeros(F, np.int64)
    one = np.ones(F, np.int64)
    two = np.full(F, 2, np.int64)
    local_s = np.concatenate([zero, one, two])
    local_e = np.concatenate([one, two, zero])
    corner_s = face_of * 3 + local_s
    corner_e = face_of * 3 + local_e

    a = np.minimum(starts, ends).astype(np.int64)
    b = np.maximum(starts, ends).astype(np.int64)
    keys = a * np.int64(nv + 1) + b
    order = np.argsort(keys, kind="stable")
    ks = keys[order]
    newrun = np.empty(len(ks), dtype=bool)
    newrun[0] = True
    newrun[1:] = ks[1:] != ks[:-1]
    run_start = np.where(newrun)[0]
    run_count = np.diff(np.append(run_start, len(ks)))
    pairs = run_start[run_count == 2]
    nm_edges = int((run_count > 2).sum())

    ncorner = F * 3
    if len(pairs):
        i0 = order[pairs]
        i1 = order[pairs + 1]
        cmin = np.where(starts == a, corner_s, corner_e)
        cmax = np.where(starts == b, corner_s, corner_e)
        src = np.concatenate([cmin[i0], cmax[i0]])
        dst = np.concatenate([cmin[i1], cmax[i1]])
        adj = sp.coo_matrix((np.ones(len(src), dtype=np.int8), (src, dst)),
                            shape=(ncorner, ncorner)).tocsr()
        _nc, comp = csgraph.connected_components(adj, directed=False)
        comp = comp.astype(np.int64)
    else:
        comp = np.arange(ncorner, dtype=np.int64)

    corner_vertex = np.empty(ncorner, dtype=np.int64)
    for L in range(3):
        corner_vertex[L::3] = tris[:, L]
    ncomp = int(comp.max()) + 1
    comp_vertex = np.zeros(ncomp, dtype=np.int64)
    comp_vertex[comp] = corner_vertex
    verts2 = verts[comp_vertex]
    tris2 = comp.reshape(F, 3)
    info = {"splits": int(len(verts2) - nv), "nm_edges_in": nm_edges,
            "faces_dropped_degenerate": dropped}
    return verts2, tris2, info


def _directed_boundary(tris, extra_edges=()):
    """Directed boundary edges as ``{start: [ends]}`` (a boundary edge is used
    exactly once).  ``extra_edges`` are synthetic directed edges (sink->source
    bridges) added on top of the real boundary."""
    starts = np.concatenate([tris[:, 0], tris[:, 1], tris[:, 2]])
    ends = np.concatenate([tris[:, 1], tris[:, 2], tris[:, 0]])
    nv = int(tris.max()) + 1
    a = np.minimum(starts, ends).astype(np.int64)
    b = np.maximum(starts, ends).astype(np.int64)
    keys = a * np.int64(nv + 1) + b
    order = np.argsort(keys, kind="stable")
    ks = keys[order]
    newrun = np.empty(len(ks), dtype=bool)
    newrun[0] = True
    newrun[1:] = ks[1:] != ks[:-1]
    run_start = np.where(newrun)[0]
    run_count = np.diff(np.append(run_start, len(ks)))
    boundary_pos = order[run_start[run_count == 1]]
    s_b = starts[boundary_pos]
    e_b = ends[boundary_pos]
    de = {}
    for s, e in zip(s_b.tolist(), e_b.tolist()):
        de.setdefault(int(s), []).append(int(e))
    for s, e in extra_edges:
        de.setdefault(int(s), []).append(int(e))
    return de


def _cut_simple(loop, loops, skipped, closed=True):
    """Append ``loop`` (a raw walk that may repeat a vertex) as simple loops,
    cutting at every repeated vertex (figure-8 / pinch split).

    ``closed`` says whether the raw walk actually returned to its start.  An
    open walk's final tail carries no closing edge, so it is NOT emitted as a
    loop (that would fabricate a phantom rim edge); only genuine sub-cycles
    bounded by a repeated vertex are kept.
    """
    stack = []
    seen = {}
    for v in loop:
        if v in seen:
            j = seen[v]
            sub = stack[j:]
            if len(sub) >= 3 and len(set(sub)) == len(sub):
                loops.append(sub)
            else:
                skipped[0] += 1
            for x in sub:
                seen.pop(x, None)
            stack = stack[:j]
        seen[v] = len(stack)
        stack.append(v)
    if len(stack) >= 3:
        if len(set(stack)) == len(stack):
            loops.append(stack)
        else:
            skipped[0] += 1
    elif stack:
        skipped[0] += 1


def boundary_loops_ex(verts, tris, extra_edges=()):
    """Like :func:`boundary_loops` but also returns the open chains.

    Returns ``(loops, skipped, chains)`` where ``chains`` are the raw walks
    that failed to close (start vertex first, dead-end/sink vertex last).
    """
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) == 0:
        return [], 0, []
    de = _directed_boundary(tris, extra_edges)
    loops = []
    chains = []
    skipped = [0]
    for s0 in list(de.keys()):
        while de.get(s0):
            cur = s0
            path = [s0]
            guard = 0
            closed = False
            while True:
                outs = de.get(cur)
                if not outs:
                    break
                nxt = outs.pop()
                if nxt == s0:
                    closed = True
                    break
                path.append(nxt)
                cur = nxt
                guard += 1
                if guard > 5 * len(tris) + 10:
                    break
            if not closed:
                chains.append(path)
            _cut_simple(path, loops, skipped, closed=closed)
    return loops, skipped[0], chains


def close_open_chains(verts, tris, max_bridges=10000, max_rounds=8):
    """Bridge each open boundary chain ``sink -> source`` so the boundary
    decomposes into closed loops.  Returns the synthetic directed bridges.

    Bridging is iterative: closing one chain can expose a new dead-end at an
    intermediate pinch vertex, so it repeats until no further chains remain
    (bounded by ``max_rounds`` / ``max_bridges``).
    """
    bridges = []
    for _ in range(max_rounds):
        _, _, chains = boundary_loops_ex(verts, tris, bridges)
        new = []
        for ch in chains:
            if len(ch) >= 2 and ch[0] != ch[-1]:
                new.append((int(ch[-1]), int(ch[0])))
        if not new:
            break
        bridges.extend(new)
        if len(bridges) >= max_bridges:
            break
    return bridges


def boundary_loops(verts, tris, extra_edges=()):
    """Extract directed boundary loops, splitting figure-8 loops at pinch
    vertices.

    Returns (loops, skipped); ``loops`` are simple vertex-index lists in the
    surface's boundary orientation, ``skipped`` counts degenerate walks (open
    chains / repeated-vertex remnants that could not be reduced to a simple
    loop).
    """
    loops, skipped, _ = boundary_loops_ex(verts, tris, extra_edges)
    return loops, skipped


# ---------------------------------------------------------------------------
# minimum-area triangulation (Liepa 2003, DP over a simple 3D polygon)
# ---------------------------------------------------------------------------
# Fairing strength as a multiple of the surrounding edge length.  Measured on
# the sphere-cap synthetic: the naive "strong small / weak large" hypothesis is
# WORSE (dev_max 1.99 at 0.3/1.5 vs 1.85 at 1.0/1.0) and the error falls
# monotonically as the offset grows (3.0 -> 1.45, 4.0 -> 1.26).  A uniform
# moderate gain is therefore used; the cylinder patch is unaffected by the
# gain (its patch has no free interior vertex).
FAIR_SMALL = 3.0     # rim ghost offset (in target edge lengths)
FAIR_LARGE = 3.0     # uniform (the tested ramp was counterproductive)
FAIR_RAMP = 30.0     # decay scale in loop length (kept for future tuning)
FAIR_DIRECT_MAX = 10 ** 9  # solve the fairing system directly (CG measured
                           # slower than sparse LU on the 400k-unknown patch)
COPLANAR_INTRA_MAX = 20000  # run the coplanar guard's intra-patch check only
                            # on patches at or below this face count (a huge
                            # refined fan is interior-disjoint by construction
                            # and makes the O(F*neighbours) test explode)
FLIP_MIN_ANGLE = 3.0  # reject a relax flip whose new triangle is thinner (deg)


def _fair_fraction(m):
    """Fairing strength as a multiple of the surrounding edge length (see the
    FAIR_* rationale: a uniform gain beat the tested loop-size ramp)."""
    return FAIR_LARGE + (FAIR_SMALL - FAIR_LARGE) * float(np.exp(-(m - 3.0) / FAIR_RAMP))


def _tri_area(pi, pk, pj):
    return 0.5 * float(np.linalg.norm(np.cross(pk - pi, pj - pi)))


def min_area_triangulation(P, cap=1200, forbidden=None):
    """Min-area DP triangulation of a simple 3D polygon P (Nx3).

    ``forbidden`` is a set of local ``(min,max)`` index pairs whose chord must
    not be used (e.g. edges that already exist in the mesh).  Returns a list of
    index triples into P, or ``[]`` when no valid triangulation exists.  For
    m > cap the O(m^3) DP is replaced by a fan (reported by the caller).
    """
    P = np.asarray(P, dtype=np.float64)
    m = len(P)

    def forb(a, b):
        if forbidden is None:
            return False
        return ((a, b) if a < b else (b, a)) in forbidden

    if m < 3:
        return []
    if m == 3:
        return [] if forb(0, 2) else [(0, 1, 2)]
    if m > cap:
        return [(0, i, i + 1) for i in range(1, m - 1)]
    # precompute the area of triangle (i, k, j) in plain Python floats; the
    # per-triangle numpy cross/norm calls dominated the DP (3.4M calls on
    # framebaroque), so the (i, j, k) area is memoised lazily.
    X = P[:, 0].tolist()
    Y = P[:, 1].tolist()
    Z = P[:, 2].tolist()

    def tri_area(i, k, j):
        ax = X[k] - X[i]
        ay = Y[k] - Y[i]
        az = Z[k] - Z[i]
        bx = X[j] - X[i]
        by = Y[j] - Y[i]
        bz = Z[j] - Z[i]
        cx = ay * bz - az * by
        cy = az * bx - ax * bz
        cz = ax * by - ay * bx
        return 0.5 * (cx * cx + cy * cy + cz * cz) ** 0.5

    INF = float("inf")
    M = [[0.0] * m for _ in range(m)]
    K = [[-1] * m for _ in range(m)]
    for gap in range(2, m):
        for i in range(0, m - gap):
            j = i + gap
            if forb(i, j):
                M[i][j] = INF
                continue
            if gap == 2:
                M[i][j] = tri_area(i, i + 1, j)
                K[i][j] = i + 1
                continue
            best = INF
            bk = -1
            Mi = M[i]
            for k in range(i + 1, j):
                if Mi[k] == INF or M[k][j] == INF:
                    continue
                w = Mi[k] + M[k][j] + tri_area(i, k, j)
                if w < best:
                    best = w
                    bk = k
            Mi[j] = best
            K[i][j] = bk
    tris = []

    def rec(i, j):
        if j - i < 2:
            return
        k = K[i][j]
        if k < 0:
            return
        tris.append((i, k, j))
        rec(i, k)
        rec(k, j)

    rec(0, m - 1)
    return tris


# ---------------------------------------------------------------------------
# patch refinement (split interior edges to the surrounding edge length)
# ---------------------------------------------------------------------------
def subdivide_patch(P, faces, boundary_edges, target, max_faces=200000,
                    threshold=None):
    """Split patch edges longer than ``threshold`` (rim edges excluded) until
    the patch edge length matches the surrounding mesh, keeping it conforming.

    ``threshold`` defaults to ``1.4 * target``.  Returns (P2, faces2, n_added).
    """
    P = [np.asarray(p, dtype=np.float64) for p in P]
    n_start = len(P)
    faces = [tuple(int(x) for x in f) for f in faces]
    bset = set(boundary_edges)
    target = max(float(target), 1e-12)
    threshold = 1.4 * target if threshold is None else float(threshold)

    def key(a, b):
        return (a, b) if a < b else (b, a)

    for _ in range(24):
        # Hard cap: an interior marked edge is shared by two faces, so it adds
        # at most two faces to the round.  Reserving half the remaining budget
        # per mark guarantees len(faces) never exceeds max_faces.
        budget = (max_faces - len(faces)) // 2
        if budget <= 0:
            break
        mid = {}
        any_marked = False
        capped = False
        for f in faces:
            a, b, c = f
            for e in ((a, b), (b, c), (c, a)):
                k = key(*e)
                if k in bset or k in mid:
                    continue
                if np.linalg.norm(P[e[0]] - P[e[1]]) > threshold:
                    if len(mid) >= budget:
                        capped = True
                        break
                    mid[k] = len(P)
                    P.append(0.5 * (P[k[0]] + P[k[1]]))
                    any_marked = True
            if capped:
                break
        if not any_marked:
            break
        new_faces = []
        for f in faces:
            a, b, c = f
            e0 = key(a, b) in mid
            e1 = key(b, c) in mid
            e2 = key(c, a) in mid
            if not (e0 or e1 or e2):
                new_faces.append(f)
                continue
            m0 = mid.get(key(a, b))
            m1 = mid.get(key(b, c))
            m2 = mid.get(key(c, a))
            pat = (int(e0), int(e1), int(e2))
            if pat == (1, 0, 0):
                new_faces += [(a, m0, c), (m0, b, c)]
            elif pat == (0, 1, 0):
                new_faces += [(b, m1, a), (m1, c, a)]
            elif pat == (0, 0, 1):
                new_faces += [(c, m2, b), (m2, a, b)]
            elif pat == (1, 1, 0):
                new_faces += [(a, m0, m1), (m0, b, m1), (a, m1, c)]
            elif pat == (0, 1, 1):
                new_faces += [(b, m1, m2), (m1, c, m2), (b, m2, a)]
            elif pat == (1, 0, 1):
                new_faces += [(c, m2, m0), (m2, a, m0), (c, m0, b)]
            else:
                new_faces += [(a, m0, m2), (m0, b, m1), (m2, m1, c),
                              (m0, m1, m2)]
        faces = new_faces
    return np.asarray(P, dtype=np.float64), faces, len(P) - n_start


def _tri_angles_aspect(P, faces):
    """Per-face minimum angle (deg) and aspect ratio for a triangle patch.

    ``aspect = longest_edge / (2 * inradius)`` (equilateral -> sqrt(3)).  Used
    both for the Liepa density test and for the quality report.
    """
    P = np.asarray(P, dtype=np.float64)
    F = np.asarray(faces, dtype=np.int64)
    if len(F) == 0:
        z = np.zeros(0)
        return z, z
    A, B, C = P[F[:, 0]], P[F[:, 1]], P[F[:, 2]]
    e0 = np.linalg.norm(B - A, axis=1)
    e1 = np.linalg.norm(C - B, axis=1)
    e2 = np.linalg.norm(A - C, axis=1)
    s = 0.5 * (e0 + e1 + e2)
    area = np.sqrt(np.maximum(s * (s - e0) * (s - e1) * (s - e2), 0.0))
    inr = np.where(s > 0.0, area / np.maximum(s, 1e-20), 1e-20)
    longest = np.maximum(np.maximum(e0, e1), e2)
    aspect = longest / (2.0 * np.maximum(inr, 1e-20))
    AB, AC = B - A, C - A             # from A
    BA, BC = A - B, C - B             # from B
    CA, CB = A - C, B - C             # from C

    def ang(u, v):
        return np.degrees(np.arccos(np.clip(
            np.sum(u * v, axis=1) /
            np.maximum(np.linalg.norm(u, axis=1) *
                       np.linalg.norm(v, axis=1), 1e-20), -1.0, 1.0)))
    a0 = ang(AB, AC)                  # angle at A
    a1 = ang(BA, BC)                  # angle at B
    a2 = ang(CA, CB)                  # angle at C
    minang = np.minimum(np.minimum(a0, a1), a2)
    return minang, aspect


def _circumradius(P, F):
    A, B, C = P[F[:, 0]], P[F[:, 1]], P[F[:, 2]]
    return (np.linalg.norm(B - A, axis=1) * np.linalg.norm(C - B, axis=1) *
            np.linalg.norm(A - C, axis=1) /
            np.maximum(2.0 * np.linalg.norm(np.cross(B - A, C - A), axis=1),
                       1e-30))


def _tri_edges(P, F):
    A, B, C = P[F[:, 0]], P[F[:, 1]], P[F[:, 2]]
    return (np.linalg.norm(B - A, axis=1), np.linalg.norm(C - B, axis=1),
            np.linalg.norm(A - C, axis=1))


def _angle_at(p, a, b):
    v1, v2 = a - p, b - p
    d = float(np.dot(v1, v2))
    n = float(np.linalg.norm(v1) * np.linalg.norm(v2))
    return 0.0 if n < 1e-20 else float(np.degrees(np.arccos(
        max(-1.0, min(1.0, d / n)))))


def _tri_min_angle(Pn, i, j, k):
    """Smallest interior angle (deg) of triangle (i, j, k)."""
    return min(_angle_at(Pn[i], Pn[j], Pn[k]),
               _angle_at(Pn[j], Pn[i], Pn[k]),
               _angle_at(Pn[k], Pn[i], Pn[j]))


def _flip_relax(Pn, faces, n_rim=0, forbidden=None, max_passes=12,
                tol_deg=0.5):
    """Delaunay edge-flip relaxation of an oriented triangle patch.

    Boundary edges (used by a single patch face, i.e. the hole rim) are never
    flipped.  Each pass applies a greedy set of non-conflicting flips that
    strictly reduce an edge's opposite-angle sum below the Delaunay bound
    (180 deg + tol); new triangles must stay non-degenerate and on the same
    side as the originals.  Returns a new face list.
    """
    faces = [tuple(int(x) for x in f) for f in faces]
    for _ in range(max_passes):
        emap = {}
        for fi, f in enumerate(faces):
            a, b, c = f
            for u, w in ((a, b), (b, c), (c, a)):
                emap.setdefault((u, w) if u < w else (w, u), []).append(fi)
        cands = []
        for (a, b), fl in emap.items():
            if len(fl) == 2:
                cands.append((a, b, fl[0], fl[1]))
        consumed = set()
        created = set()
        n_flip = 0
        for a, b, i1, i2 in cands:
            if i1 in consumed or i2 in consumed:
                continue
            f1, f2 = faces[i1], faces[i2]
            if a not in f1 or b not in f1 or a not in f2 or b not in f2:
                continue

            def der(f):
                for i in range(3):
                    if f[i] == a and f[(i + 1) % 3] == b:
                        return True
                    if f[i] == b and f[(i + 1) % 3] == a:
                        return False
                return None
            d1, d2 = der(f1), der(f2)
            if d1 is None or d1 == d2:
                continue
            if not d1:
                a, b = b, a
            c = next(x for x in f1 if x != a and x != b)
            d = next(x for x in f2 if x != a and x != b)
            if len({a, b, c, d}) < 4:
                continue
            nk = (c, d) if c < d else (d, c)
            if nk in emap or nk in created:   # would duplicate a patch edge
                continue
            if forbidden and c < n_rim and d < n_rim and nk in forbidden:
                continue
            if _angle_at(Pn[c], Pn[a], Pn[b]) + \
                    _angle_at(Pn[d], Pn[a], Pn[b]) <= 180.0 + tol_deg:
                continue
            no1 = np.cross(Pn[b] - Pn[a], Pn[c] - Pn[a])
            no2 = np.cross(Pn[a] - Pn[b], Pn[d] - Pn[b])
            nn1 = np.cross(Pn[d] - Pn[a], Pn[c] - Pn[a])
            nn2 = np.cross(Pn[b] - Pn[d], Pn[c] - Pn[d])
            if (float(np.dot(nn1, no1)) <= 0.0 or
                    float(np.dot(nn2, no2)) <= 0.0 or
                    float(np.linalg.norm(nn1)) < 1e-24 or
                    float(np.linalg.norm(nn2)) < 1e-24):
                continue
            if (_tri_min_angle(Pn, a, d, c) < FLIP_MIN_ANGLE or
                    _tri_min_angle(Pn, d, b, c) < FLIP_MIN_ANGLE):
                continue
            faces[i1] = (a, d, c)
            faces[i2] = (d, b, c)
            consumed.add(i1)
            consumed.add(i2)
            created.add(nk)
            n_flip += 1
        if n_flip == 0:
            break
    return faces


def _valence_relax(Pn, faces, n_rim, forbidden=None, max_passes=8):
    """Valence regularisation: flip an interior edge when it strictly reduces
    the squared valence error (interior target 6, rim target 4) and the new
    triangles stay non-degenerate and same-side."""
    faces = [tuple(int(x) for x in f) for f in faces]
    nv = len(Pn)
    for _ in range(max_passes):
        val = {}
        for f in faces:
            for x in f:
                val[x] = val.get(x, 0) + 1
        emap = {}
        for fi, f in enumerate(faces):
            a, b, c = f
            for u, w in ((a, b), (b, c), (c, a)):
                emap.setdefault((u, w) if u < w else (w, u), []).append(fi)

        cands = [k for k, fl in emap.items() if len(fl) == 2]
        consumed = set()
        created = set()
        n_flip = 0
        for (a, b) in cands:
            fl = emap[(a, b)]
            i1, i2 = fl
            if i1 in consumed or i2 in consumed:
                continue
            f1, f2 = faces[i1], faces[i2]
            if a not in f1 or b not in f1 or a not in f2 or b not in f2:
                continue

            def der(f):
                for i in range(3):
                    if f[i] == a and f[(i + 1) % 3] == b:
                        return True
                    if f[i] == b and f[(i + 1) % 3] == a:
                        return False
                return None
            d1, d2 = der(f1), der(f2)
            if d1 is None or d1 == d2:
                continue
            if not d1:
                a, b = b, a
            c = next(x for x in f1 if x != a and x != b)
            d = next(x for x in f2 if x != a and x != b)
            if len({a, b, c, d}) < 4:
                continue
            nk = (c, d) if c < d else (d, c)
            if nk in emap or nk in created:   # would duplicate a patch edge
                continue
            if forbidden and c < n_rim and d < n_rim and nk in forbidden:
                continue
            before = (val[a] - (4 if a < n_rim else 6)) ** 2 + \
                     (val[b] - (4 if b < n_rim else 6)) ** 2 + \
                     (val[c] - (4 if c < n_rim else 6)) ** 2 + \
                     (val[d] - (4 if d < n_rim else 6)) ** 2
            after = ((val[a] - 1 - (4 if a < n_rim else 6)) ** 2 +
                     (val[b] - 1 - (4 if b < n_rim else 6)) ** 2 +
                     (val[c] + 1 - (4 if c < n_rim else 6)) ** 2 +
                     (val[d] + 1 - (4 if d < n_rim else 6)) ** 2)
            if after >= before:
                continue
            no1 = np.cross(Pn[b] - Pn[a], Pn[c] - Pn[a])
            no2 = np.cross(Pn[a] - Pn[b], Pn[d] - Pn[b])
            nn1 = np.cross(Pn[d] - Pn[a], Pn[c] - Pn[a])
            nn2 = np.cross(Pn[b] - Pn[d], Pn[c] - Pn[d])
            if (float(np.dot(nn1, no1)) <= 0.0 or
                    float(np.dot(nn2, no2)) <= 0.0 or
                    float(np.linalg.norm(nn1)) < 1e-24 or
                    float(np.linalg.norm(nn2)) < 1e-24):
                continue
            if (_tri_min_angle(Pn, a, d, c) < FLIP_MIN_ANGLE or
                    _tri_min_angle(Pn, d, b, c) < FLIP_MIN_ANGLE):
                continue
            faces[i1] = (a, d, c)
            faces[i2] = (d, b, c)
            val[a] -= 1
            val[b] -= 1
            val[c] += 1
            val[d] += 1
            consumed.add(i1)
            consumed.add(i2)
            created.add(nk)
            n_flip += 1
        if n_flip == 0:
            break
    return faces


def _cleanup_slivers(P, faces, n_rim, min_angle_deg=15.0, max_iter=500):
    """Remove sliver/degenerate patch triangles by edge collapse of patch-only
    edges, without ever moving a distinct original (rim) vertex.

    A triangle whose minimum angle is below ``min_angle_deg`` is a sliver.  Its
    shortest edge is collapsed (the two endpoints merged): a new vertex is
    merged into a rim vertex, a new vertex into a new vertex, or -- for a
    zero-length edge (the crack / pinch case where two coincident rim vertices
    were split by the non-manifold fan explosion) -- two coincident rim vertices.
    Two *distinct* rim vertices are never merged; a sliver made only of distinct
    rim vertices (a near-collinear rim hole that cannot be repaired without
    moving original geometry) is dropped, leaving that hole open.

    Returns ``(P2, faces2)`` with contiguous indices (rim 0..n_rim-1 kept in
    order, surviving new vertices compacted after them).
    """
    P = [np.asarray(p, dtype=np.float64) for p in P]
    faces = [list(int(x) for x in f) for f in faces]
    for _ in range(max_iter):
        if not faces:
            break
        F = np.asarray(faces, dtype=np.int64)
        minang, _ = _tri_angles_aspect(np.asarray(P), F)
        worst = int(np.argmin(minang))
        if minang[worst] >= min_angle_deg:
            break
        a, b, c = faces[worst]
        edges = ((a, b), (b, c), (c, a))
        lens = [float(np.linalg.norm(P[x] - P[y])) for x, y in edges]
        u, v = edges[int(np.argmin(lens))]
        u_rim, v_rim = u < n_rim, v < n_rim
        if u_rim and v_rim and float(np.linalg.norm(P[u] - P[v])) > 1e-12:
            # two distinct rim vertices: cannot collapse without moving the
            # original rim; drop the sliver (a near-collinear rim hole).
            faces.pop(worst)
            continue
        if u_rim and not v_rim:
            keep, drop = u, v
        elif v_rim and not u_rim:
            keep, drop = v, u
        else:
            keep, drop = u, v
        for f in faces:
            for k in range(3):
                if f[k] == drop:
                    f[k] = keep
        faces = [f for f in faces if len(set(f)) == 3]
    used = set(range(n_rim))
    for f in faces:
        used.update(f)
    used = sorted(used)
    remap = {old: new for new, old in enumerate(used)}
    P2 = np.asarray([P[i] for i in used], dtype=np.float64)
    faces2 = [tuple(remap[x] for x in f) for f in faces]
    return P2, faces2


def refine_patch(P, faces, n_rim, target, max_faces=200000, rounds=6,
                 flips=True, forbidden=None, med_edge=None, surround=None):
    """Liepa-style patch refinement (density bisection + Delaunay/valence flips).

    Two paths:

    * *Needle-fan* holes (a small hole whose rim is far finer than the
      surrounding mesh, e.g. thingi10k_1038441 with rim 0.008 vs 0.94) are
      refined by the circumradius/centroid scheme: repeat until no triangle
      remains denser than the target, splitting a triangle whose circumradius
      ``R`` exceeds ``sqrt(2) * target`` at its centroid (a conforming 1-to-3
      split), then relaxing with Delaunay and valence flips.

    * Every other hole is densified by conforming edge bisection
      (:func:`subdivide_patch`): split every edge longer than the density
      threshold at its midpoint (rim edges excluded), then relax with
      Delaunay/valence flips; repeat until stable, at the local surrounding
      edge length (never finer than ``surround``).

    Edge bisection is used in place of a pure centroid split: a centroid split
    never shortens a triangle's longest edge, so a sliver keeps exceeding the
    density threshold and the patch grows 3^rounds (measured: the m=7 loop on
    artec_metal-nut exploded past 150k faces).  Longest-edge bisection halves
    the offending edge and converges, after which the flips recover shape.
    Returns ``(P2, faces2)``.
    """
    P = [np.asarray(p, dtype=np.float64) for p in P]
    faces = [tuple(int(x) for x in f) for f in faces]
    if len(faces) < 3:
        return np.asarray(P, dtype=np.float64), faces
    target = max(float(target), 1e-12)
    thresh = 2.0 ** 0.5 * target
    boundary_edges = set()
    for i in range(n_rim):
        a, b = i, (i + 1) % n_rim
        boundary_edges.add((a, b) if a < b else (b, a))

    med = float(med_edge) if med_edge else 0.0
    density = max(target, float(surround)) if surround else target
    surr = float(surround) if surround else target
    rim = np.asarray(P[:n_rim], dtype=np.float64)
    rim_len = np.linalg.norm(np.roll(rim, -1, axis=0) - rim, axis=1)
    perim = float(rim_len.sum())
    rim_max = float(rim_len.max())
    # Edge bisection whenever the hole must be densified rather than merely
    # have its slivers repaired.  A large hole (long perimeter), a non-uniform
    # rim (a rim edge already exceeds the density threshold and can never be
    # shortened by a centroid split), or a rim that is not extremely finer than
    # its surroundings (so it is not a needle fan at all).
    needs_bisect = ((med > 0.0 and perim > 1.5 * med) or
                    rim_max > thresh or
                    target > 0.05 * surr)
    if needs_bisect:
        bisect_thresh = 2.0 ** 0.5 * density
        for _ in range(rounds):
            n0 = len(faces)
            P, faces, added = subdivide_patch(
                P, faces, boundary_edges, density, max_faces=max_faces,
                threshold=bisect_thresh)
            if added == 0 and len(faces) == n0:
                break
        return np.asarray(P, dtype=np.float64), faces

    # needle-fan: circumradius/centroid 1-to-3 split, then flips, until stable.
    # The centroid scheme needs its own (larger) convergence budget than the
    # bisection loop above, since a centroid split only halves a circumradius
    # gradually (the original scheme used 40 rounds).
    for _ in range(40):
        if len(faces) < 3:
            break
        F = np.asarray(faces, dtype=np.int64)
        R = _circumradius(np.asarray(P), F)
        e0, e1, e2 = _tri_edges(np.asarray(P), F)
        longest = np.maximum(np.maximum(e0, e1), e2)
        cand = np.where((R > thresh) | (longest > thresh))[0]
        if len(cand):
            room = max_faces - len(faces)
            if room <= 0:
                break
            if len(cand) * 2 > room:
                cand = cand[np.argsort(R[cand])[::-1][: max(room // 2, 1)]]
            split_set = set(int(i) for i in cand)
            new_faces = []
            for fi, (a, b, c) in enumerate(faces):
                if fi in split_set:
                    g = len(P)
                    P.append((P[a] + P[b] + P[c]) / 3.0)
                    new_faces += [(a, b, g), (b, c, g), (c, a, g)]
                else:
                    new_faces.append((a, b, c))
            faces = new_faces
        if flips:
            faces = _flip_relax(np.asarray(P), faces, n_rim, forbidden)
            faces = _valence_relax(np.asarray(P), faces, n_rim, forbidden)
        if not len(cand):
            break
    return np.asarray(P, dtype=np.float64), faces


def _vertex_adjacency(t, nv):
    """Vectorized CSR adjacency.  Returns ``(offsets, neighbors)`` such that the
    neighbours of vertex ``i`` are ``neighbors[offsets[i]:offsets[i+1]]``."""
    s = np.concatenate([t[:, 0], t[:, 1], t[:, 2]]).astype(np.int64)
    e = np.concatenate([t[:, 1], t[:, 2], t[:, 0]]).astype(np.int64)
    order = np.argsort(s, kind="stable")
    s_sorted = s[order]
    e_sorted = e[order]
    counts = np.bincount(s_sorted, minlength=nv)
    offsets = np.zeros(nv + 1, dtype=np.int64)
    np.cumsum(counts, out=offsets[1:])
    return offsets, e_sorted


def _vertex_normals(verts, tris):
    """Area-weighted vertex normals."""
    fn = np.cross(verts[tris[:, 1]] - verts[tris[:, 0]],
                  verts[tris[:, 2]] - verts[tris[:, 0]])
    vn = np.zeros_like(verts, dtype=np.float64)
    for L in range(3):
        np.add.at(vn, tris[:, L], fn)
    n = np.linalg.norm(vn, axis=1)
    n[n == 0] = 1.0
    return vn / n[:, None]


# ---------------------------------------------------------------------------
# fairing (discrete biharmonic / thin-plate with C1 ghost boundary)
# ---------------------------------------------------------------------------
def fair_patch(P, faces, n_boundary, ghost_pos):
    """Fair the interior patch vertices with a biharmonic solve.

    P: (n,3) flap vertices (first ``n_boundary`` are the fixed hole rim).
    ghost_pos: (n_boundary,3) C1 ghost positions (fixed) connected one-to-one
    to the rim vertices; they carry the surrounding surface's slope/curvature.
    """
    if len(P) <= n_boundary:
        return P
    P = P.copy()
    ng = n_boundary
    ntot = len(P) + ng
    F = np.asarray(faces, dtype=np.int64)
    e = np.concatenate([F[:, [0, 1]], F[:, [1, 2]], F[:, [2, 0]]], axis=0)
    gidx = np.arange(ng, dtype=np.int64)
    rows = np.concatenate([e[:, 0], e[:, 1], gidx, len(P) + gidx])
    cols = np.concatenate([e[:, 1], e[:, 0], len(P) + gidx, gidx])
    W = sp.coo_matrix((np.ones(len(rows)), (rows, cols)),
                      shape=(ntot, ntot)).tocsr()
    W.data[:] = 1.0
    deg = np.asarray(W.sum(1)).ravel()
    L = sp.diags(deg) - W
    M = (L.T @ L).tocsc()

    X = np.zeros((ntot, 3))
    X[:len(P)] = P
    X[len(P):ntot] = ghost_pos
    unknown = np.arange(n_boundary, len(P))
    fixed = np.concatenate([np.arange(0, n_boundary),
                            np.arange(len(P), ntot)])
    A = M[unknown][:, unknown] + sp.identity(len(unknown), format="csc") * 1e-9
    rhs = -(M[unknown][:, fixed] @ X[fixed])
    try:
        if len(unknown) > FAIR_DIRECT_MAX:
            sol = np.empty((len(unknown), 3))
            Ac = A.tocsc()
            for _c in range(3):
                sol[:, _c] = spla.cg(Ac, rhs[:, _c], rtol=1e-7, maxiter=3000)[0]
        else:
            sol = spla.spsolve(A.tocsc(), rhs)
    except Exception:
        sol = spla.lsqr(A.tocsc(), rhs)[0]
    sol = np.asarray(sol)
    if sol.ndim == 1:
        sol = sol.reshape(-1, 3)
    P[unknown] = sol
    return P


def weld_open_chains(v, t, max_rounds=12, max_merge_frac=None, sites=None,
                     move_tol=0.1, protect_below=None):
    """Close boundary cracks by welding each open chain's source to its sink.

    An unbalanced boundary vertex (out != in) marks a crack end: two nearly
    coincident vertices that should be one.  Merging the two ends of each open
    chain balances the boundary so it decomposes into simple loops, without
    adding any face across the hole.  Iterated because one merge can expose a
    new dead-end.  ``max_merge_frac`` (fraction of the bbox diagonal) skips a
    merge that would move the kept vertex further than that (distortion guard).
    Returns ``(v2, t2, merges)``.
    """
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    merges = 0
    diag = None
    if max_merge_frac is not None:
        ext = v.max(axis=0) - v.min(axis=0)
        diag = float(np.linalg.norm(ext))
        if diag <= 0:
            max_merge_frac = None
    for _ in range(max_rounds):
        _, _, chains = boundary_loops_ex(v, t)
        pairs = []
        for ch in chains:
            if len(ch) >= 2 and ch[0] != ch[-1]:
                a, b = int(ch[0]), int(ch[-1])
                gap = float(np.linalg.norm(v[a] - v[b]))
                if move_tol is not None and gap > move_tol:
                    continue
                if max_merge_frac is not None and \
                        gap > max_merge_frac * diag:
                    continue
                pairs.append((a, b))
        if not pairs:
            break
        n = len(v)
        parent = list(range(n))

        def find(x):
            while parent[x] != x:
                parent[x] = parent[parent[x]]
                x = parent[x]
            return x

        for a, b in pairs:
            ra, rb = find(a), find(b)
            if ra != rb:
                parent[ra] = rb
                if sites is not None:
                    is_orig = protect_below is not None and \
                        (a < protect_below or b < protect_below)
                    sites.append((v[a].copy(), v[b].copy(),
                                  float(np.linalg.norm(v[a] - v[b])),
                                  bool(is_orig)))
        roots = np.array([find(x) for x in range(n)], dtype=np.int64)
        used = np.unique(t)
        remap = {}
        vlist = []
        for x in used.tolist():
            r = int(roots[x])
            if r not in remap:
                remap[r] = len(vlist)
                vlist.append(v[x])
        t_new = np.array([[remap[int(roots[int(a)])] for a in f] for f in t],
                         dtype=np.int64)
        changed = len(vlist) < n
        v = np.array(vlist, dtype=np.float64) if vlist else v
        t = t_new
        if changed:
            merges += 1
        else:
            break
    return v, t, merges


def _boundary_balance(t):
    """Return ``(unbalanced_vertices, nm_edges)`` for a triangle array.

    ``unbalanced`` counts boundary vertices whose boundary out-degree differs
    from its in-degree (open-chain ends / pinch sources-sinks); ``nm_edges``
    counts edges used by more than two faces.
    """
    t = np.asarray(t, dtype=np.int64)
    if len(t) == 0:
        return 0, 0
    s = np.concatenate([t[:, 0], t[:, 1], t[:, 2]])
    e = np.concatenate([t[:, 1], t[:, 2], t[:, 0]])
    nv = int(t.max()) + 1
    a = np.minimum(s, e).astype(np.int64)
    b = np.maximum(s, e).astype(np.int64)
    k = a * np.int64(nv + 1) + b
    uq, ct = np.unique(k, return_counts=True)
    nm_edges = int((ct > 2).sum())
    bs = set(int(x) for x in uq[ct == 1])
    outd = {}
    ind = {}
    for ss, ee, kk in zip(s.tolist(), e.tolist(), k.tolist()):
        if kk in bs:
            outd[ss] = outd.get(ss, 0) + 1
            ind[ee] = ind.get(ee, 0) + 1
    allv = set(outd) | set(ind)
    unbalanced = sum(1 for x in allv if outd.get(x, 0) != ind.get(x, 0))
    return unbalanced, nm_edges


def normalize_boundary(v, t, max_iter=8, max_merge_frac=None, move_tol=0.1,
                       protect_below=None):
    """Make the boundary a set of simple, closed, manifold loops.

    Alternates the vertex-fan split (`split_non_manifold`: a pinch vertex is
    duplicated once per incident face fan) with the crack weld
    (`weld_open_chains`: each open chain's source is merged onto its sink) until
    no boundary vertex is unbalanced and no edge carries more than two faces.
    The split creates the fan copies; the weld then balances the dangling ends
    the split exposes, so the pair converges where either alone stalls.

    Returns ``(v2, t2, info)``.
    """
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    info = {"iterations": 0, "splits": 0, "welds": 0, "weld_sites": []}
    best = (v, t, (10 ** 9, 10 ** 9))
    for it in range(1, max_iter + 1):
        v, t, si = split_non_manifold(v, t)
        sites = []
        v, t, wm = weld_open_chains(v, t, max_rounds=16, sites=sites, \
                                    max_merge_frac=max_merge_frac, \
                                    move_tol=move_tol, \
                                    protect_below=protect_below)
        info["weld_sites"].extend(sites)
        ub, nm = _boundary_balance(t)
        info.update(iterations=it, splits=si["splits"], welds=wm,
                    unbalanced=ub, nm_edges=nm)
        score = (ub, nm)
        if score < best[2]:
            best = (v, t, score)
        if ub == 0 and nm == 0:
            return v, t, info
    return best[0], best[1], info


def separate_coincident_stl(v, shift=None, protect_below=None):
    """Make distinct vertices float32-distinct so an STL reload keeps topology.

    STL stores no vertex sharing, so a loader re-welds positions that are
    bit-identical in float32.  The fan split (`split_non_manifold`) and the
    fill leave distinct vertices at the same coordinate on purpose; without a
    nudge the reload merges them back and reintroduces open / non-manifold
    edges.  Each extra copy is moved a few float32 ULPs (sub-micron) while its
    index and every face reference stay untouched, so the in-memory topology
    is unchanged.

    Returns ``(v2, moved)``.
    """
    v = np.asarray(v, dtype=np.float64).copy()
    f = np.asarray(v, np.float32)
    order = np.lexsort((f[:, 2], f[:, 1], f[:, 0]))
    fs = f[order]
    same = np.empty(len(fs), dtype=bool)
    same[0] = False
    same[1:] = np.all(fs[1:] == fs[:-1], axis=1)
    starts = np.flatnonzero(~same)
    counts = np.diff(np.append(starts, len(fs)))
    step = np.arange(len(fs)) - np.repeat(starts, counts)
    nudge_mask = step > 0
    if protect_below is not None:
        kept = order[starts]
        run_id = np.repeat(np.arange(len(starts)), counts)
        nudge_mask &= (kept[run_id] >= protect_below)
    nudge_idx = order[nudge_mask]
    nudge_step = step[nudge_mask]
    if shift is None:
        shift = float(np.linalg.norm(v.max(0) - v.min(0))) * 1e-5
    for k, i in enumerate(nudge_idx.tolist()):
        s = int(nudge_step[k])
        ang = s * 2.39996323
        z = 1.0 - 2.0 * ((s % 997) / 997.0)
        r = float(np.sqrt(max(0.0, 1.0 - z * z)))
        d = np.array([r * np.cos(ang), r * np.sin(ang), z])
        v[i] += shift * d
    return v, int(len(nudge_idx))


def drop_lone_triangles(v, t):
    """Drop isolated single-triangle components (debris that cannot be filled
    without duplicating the triangle).  Keeps everything else byte-identical.

    Returns ``(v2, t2, dropped)`` where ``dropped`` is the number of removed
    triangles; the input is returned unchanged when that would remove all
    geometry.
    """
    t = np.asarray(t, dtype=np.int64)
    if len(t) <= 1:
        return v, t, 0
    # vectorized face-adjacency component labelling: two faces share a
    # component when they share an edge used by exactly two faces.
    e0 = np.concatenate([t[:, 0], t[:, 1], t[:, 2]])
    e1 = np.concatenate([t[:, 1], t[:, 2], t[:, 0]])
    a = np.minimum(e0, e1).astype(np.int64)
    b = np.maximum(e0, e1).astype(np.int64)
    nv = int(t.max()) + 1
    keys = a * np.int64(nv + 1) + b
    face_of = np.tile(np.arange(len(t), dtype=np.int64), 3)
    order = np.argsort(keys, kind="stable")
    ks = keys[order]
    fo = face_of[order]
    newrun = np.empty(len(ks), dtype=bool)
    newrun[0] = True
    newrun[1:] = ks[1:] != ks[:-1]
    run_start = np.where(newrun)[0]
    run_count = np.diff(np.append(run_start, len(ks)))
    pair_start = run_start[run_count == 2]
    f1 = fo[pair_start]
    f2 = fo[pair_start + 1]
    adj = sp.coo_matrix((np.ones(len(f1), dtype=np.int8), (f1, f2)),
                        shape=(len(t), len(t))).tocsr()
    _nc, comp = csgraph.connected_components(adj, directed=False)
    comp = comp.astype(np.int64)
    size = np.bincount(comp)
    keep = size[comp] >= 2
    if not keep.any() or keep.all():
        return v, t, int((~keep).sum())
    t2 = t[keep]
    used = np.unique(t2)
    remap = -np.ones(int(t.max()) + 1, np.int64)
    remap[used] = np.arange(len(used))
    return np.asarray(v)[used], remap[t2], int((~keep).sum())


# ---------------------------------------------------------------------------
# top level
# ---------------------------------------------------------------------------
def flap_fill_python(verts, tris, *, refine=True, fair=True, max_loop=1200,
                     max_faces=400000, collect_quality=False,
                     drop_lone_tris=True, weld_cracks=True, split_nm=True,
                     separate_stl=True, bridge_open_chains=False,
                     weld_max_frac=0.02, orient="reverse", avoid_coplanar=True,
                     coplanar_frac=0.05, coplanar_cos=0.99,
                     sliver_cleanup=False):
    """Cover every simple boundary loop with a fair surface flap.

    The pure-numpy reference oracle.  ``flap_fill`` dispatches to the Rust port
    when the extension exposes it; this function remains the fallback and the
    parity oracle for the port.

    ``collect_quality`` adds aggregate patch triangle-quality percentiles
    (min-angle p5/p50, aspect p50/p95) for the DP patch *before* refinement and
    the committed patch *after*, under the report keys ``quality_before`` /
    ``quality_after``.  Returns (verts_out, tris_out, report).
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    report = {"loops_found": 0, "loops_filled": 0, "loops_skipped": 0,
              "loops_skipped_nm": 0, "loops_skipped_sheet": 0,
              "loops_fan_fallback": 0, "loops_seeded": 0, "loops_refined": 0,
              "patch_faces": 0, "new_vertices": 0, "large_loops_fan": 0,
              "refine": bool(refine), "fair": bool(fair), "split": None,
              "loops_skipped_coplanar": 0, "loops_skipped_flat": 0,
              "non_simple_reported": 0, "errors": [], "loop_log": []}
    _q_before_min, _q_before_asp = [], []
    _q_after_min, _q_after_asp = [], []
    if weld_cracks:
        v2, t2, norm = normalize_boundary(verts, tris, \
                                          max_merge_frac=weld_max_frac, \
                                          move_tol=0.1, \
                                          protect_below=len(verts))
        report["boundary_normalize"] = norm
        report["welded_cracks"] = norm.get("welds", 0)
        split_info = {"splits": norm.get("splits", 0), "nm_edges_in": 0}
    elif split_nm:
        v2, t2, split_info = split_non_manifold(verts, tris)
    else:
        v2, t2, split_info = np.asarray(verts, dtype=np.float64), \
            np.asarray(tris, dtype=np.int64), {"splits": 0, "nm_edges_in": 0}
    if drop_lone_tris:
        v2, t2, dropped_lone = drop_lone_triangles(v2, t2)
        report["dropped_lone_triangles"] = dropped_lone
    report["split"] = split_info

    e = np.concatenate([v2[t2[:, 0]] - v2[t2[:, 1]],
                        v2[t2[:, 1]] - v2[t2[:, 2]],
                        v2[t2[:, 2]] - v2[t2[:, 0]]], axis=0)
    el = np.linalg.norm(e, axis=1)
    med_edge = float(np.median(el)) if len(el) else 1.0

    bridges = []
    if bridge_open_chains:
        bridges = close_open_chains(v2, t2)
        report["chains_bridged"] = len(bridges)
    loops, skipped = boundary_loops(v2, t2, extra_edges=bridges)
    report["loops_found"] = len(loops) + skipped
    report["loops_skipped"] = skipped
    report["non_simple_reported"] = skipped
    out_v = list(v2)
    out_t = [tuple(f.tolist()) for f in t2]

    extra_ref = []

    vadj_off, vadj_nb = _vertex_adjacency(t2, len(v2))
    vnorm = _vertex_normals(v2, t2)

    # Vectorized edge map: gcount (edge -> count), orig_edge_face (edge ->
    # first face), plus the sorted arrays kept for the lazy m==3 sheet lookup.
    _fe0 = np.concatenate([t2[:, 0], t2[:, 1], t2[:, 2]]).astype(np.int64)
    _fe1 = np.concatenate([t2[:, 1], t2[:, 2], t2[:, 0]]).astype(np.int64)
    _fa = np.minimum(_fe0, _fe1)
    _fb = np.maximum(_fe0, _fe1)
    _env = len(v2)
    _fkeys = _fa * np.int64(_env + 1) + _fb
    _fface = np.tile(np.arange(len(t2), dtype=np.int64), 3)
    _forder = np.argsort(_fkeys, kind="stable")
    _fks = _fkeys[_forder]
    _ffo = _fface[_forder]
    _fnew = np.empty(len(_fks), dtype=bool)
    _fnew[0] = True
    _fnew[1:] = _fks[1:] != _fks[:-1]
    _frs = np.where(_fnew)[0]
    _frc = np.diff(np.append(_frs, len(_fks)))
    _fuq = _fks[_frs]
    # Consumers key these maps by (min,max) TUPLES, so decode the packed
    # int64 keys back to tuples here (the packed form is only used for the
    # sorted-array searchsorted lookups below).
    _fuq_a = (_fuq // np.int64(_env + 1)).tolist()
    _fuq_b = (_fuq % np.int64(_env + 1)).tolist()
    _tkeys = list(zip(_fuq_a, _fuq_b))
    gcount = dict(zip(_tkeys, _frc.tolist()))
    orig_edge_face = dict(zip(_tkeys, _ffo[_frs].tolist()))
    _lab, _volc, _areac, _flatc = _component_stats(v2, t2, _frc, _ffo, _frs)
    nloops_c = np.zeros(len(_volc), np.int64)
    for _lp in loops:
        _a, _b = int(_lp[0]), int(_lp[1])
        _kk = (_a if _a < _b else _b) * np.int64(_env + 1) + \
            (_b if _a < _b else _a)
        _idx = int(np.searchsorted(_fuq, _kk))
        if _idx < len(_fuq) and _fuq[_idx] == _kk:
            nloops_c[int(_lab[int(_ffo[_frs[_idx]])])] += 1
    ZERO_VOL_FRAC = 1e-3
    for loop in loops:
        if len(loop) > max_loop:
            report["large_loops_fan"] += 1
        loop_arr = np.asarray(loop, dtype=np.int64)
        pts = v2[loop_arr]
        m = len(pts)
        forb = set()
        for a in range(m):
            ga = int(loop_arr[a])
            for b in range(a + 1, m):
                gb = int(loop_arr[b])
                k = (ga, gb) if ga < gb else (gb, ga)
                if gcount.get(k, 0) >= 2:
                    forb.add((a, b))
        tri_local = min_area_triangulation(pts, forbidden=forb)
        is_fan = False
        if tri_local:
            P = np.array(pts, dtype=np.float64)
        else:
            # min-area DP is blocked by existing interior chords: fall back to a
            # centroid fan (no boundary-boundary chords, so never blocked).
            cm_pt = pts.mean(axis=0)
            P = np.vstack([pts, cm_pt]).astype(np.float64)
            tri_local = [(i, (i + 1) % m, m) for i in range(m)]
            is_fan = True
            report["loops_fan_fallback"] += 1
        faces = [(int(a), int(b), int(c)) for (a, b, c) in tri_local]

        # Liepa target: the local (boundary) edge length.  Using the
        # surrounding faces' edges instead breaks badly when the hole rim is
        # much finer than the mesh around it (thingi10k_1038441: rim 0.008 vs
        # surrounding 0.94), where no triangle would ever be refined.
        rim_len = np.linalg.norm(np.roll(pts, -1, axis=0) - pts, axis=1)
        target = float(np.median(rim_len)) if len(rim_len) else med_edge
        log = {"m": int(m), "fan": bool(is_fan),
               "large": bool(len(loop) > max_loop), "action": None}
        report["loop_log"].append(log)

        # A loop on a flat coplanar sheet must stay open: capping it would
        # close the sheet into a zero-volume double-face shell (agy: the 25
        # spurious parts Flap fed Graft on framebaroque).  Graft handles it.
        _g0, _g1 = int(loop_arr[0]), int(loop_arr[1])
        _fi = orig_edge_face.get((_g0, _g1) if _g0 < _g1 else (_g1, _g0))
        if _fi is not None and _flatc[int(_lab[_fi])]:
            report["loops_skipped_flat"] = report.get("loops_skipped_flat", 0) + 1
            log["action"] = "flat"
            continue

        if collect_quality:
            qa, qb = _tri_angles_aspect(P, faces)
            _q_before_min.append(qa)
            _q_before_asp.append(qb)

        # Local surrounding density: median length of the mesh edges in the
        # 2-ring around the rim (rim edges excluded).  This caps the patch
        # density so a large hole is filled at the *surrounding* density,
        # never finer: the rim's own (often far finer) edge length would
        # otherwise over-refine huge holes.
        rim_set = set(int(x) for x in loop_arr)
        ring1 = set()
        for i in range(m):
            gv = int(loop_arr[i])
            ring1.update(int(nb) for nb in
                         vadj_nb[vadj_off[gv]:vadj_off[gv + 1]]
                         if int(nb) not in rim_set)
        seen_e = set()
        surr_lens = []

        def _add_edges(gv):
            gv = int(gv)
            for nb in vadj_nb[vadj_off[gv]:vadj_off[gv + 1]]:
                nb = int(nb)
                if nb in rim_set or nb == gv:
                    continue
                k = (gv, nb) if gv < nb else (nb, gv)
                if k in seen_e:
                    continue
                seen_e.add(k)
                surr_lens.append(float(np.linalg.norm(v2[gv] - v2[nb])))

        for gv in loop_arr:
            _add_edges(gv)
        for gv in ring1:
            _add_edges(gv)
        local_surr = float(np.median(surr_lens)) if surr_lens else med_edge

        # Liepa refinement: needle-fan holes are centroid-split (circumradius >
        # sqrt(2) * target) and relaxed with Delaunay/valence flips; all other
        # holes are densified by conforming edge bisection at the surrounding
        # density.  See refine_patch.
        n_before_refine = len(P)
        if refine or fair:
            P, faces = refine_patch(P, faces, m, target, max_faces=max_faces,
                                    forbidden=forb, med_edge=med_edge,
                                    surround=local_surr)
        was_seeded = len(P) > n_before_refine

        # consistent orientation: compare the patch face sharing the first loop
        # edge with the original surface face sharing that same edge.
        if orient == "reverse":
            # The loop is the surface's directed boundary (a->b); a consistent
            # patch must traverse every rim edge the other way (b->a), so the
            # patch triangulated in loop order is always reversed.
            faces = [(a, c, b) for (a, b, c) in faces]
            report["orient_reversed"] = report.get("orient_reversed", 0) + 1
        elif m >= 2:
            g0, g1 = int(loop_arr[0]), int(loop_arr[1])
            ofi = orig_edge_face.get((g0, g1) if g0 < g1 else (g1, g0))
            if ofi is not None:
                fo = t2[ofi]
                n_orig = np.cross(v2[fo[1]] - v2[fo[0]], v2[fo[2]] - v2[fo[0]])
                lk = (0, 1) if 0 < 1 else (1, 0)
                for (a, b, c) in faces:
                    if {a, b, c}.issuperset(set(lk)):
                        n_flap = np.cross(P[b] - P[a], P[c] - P[a])
                        if float(np.dot(n_orig, n_flap)) < 0:
                            faces = [(a, c, b) for (a, b, c) in faces]
                        break

        if fair and len(P) > m:
            loop_set = set(int(x) for x in loop_arr)
            centroid = pts.mean(axis=0)
            ghost_pos = np.zeros((m, 3))
            ell = max(float(target), 1e-12) * _fair_fraction(m)
            for i in range(m):
                gv = int(loop_arr[i])
                p = v2[gv]
                nrm = vnorm[gv]
                d = centroid - p
                dn = np.linalg.norm(d)
                if dn < 1e-12:
                    ghost_pos[i] = p
                    continue
                d = d / dn
                tng = d - float(np.dot(d, nrm)) * nrm   # inward tangent
                tn = np.linalg.norm(tng)
                ghost_pos[i] = p - ell * (tng / tn) if tn > 1e-9 else p
            P = fair_patch(P, faces, m, ghost_pos)

        # Final sliver cleanup: remove degenerate/sliver patch triangles (the
        # DP can emit zero-area triangles on crack/pinch loops and thin slivers
        # on near-collinear rims).  Restricted to patch-only edges; never moves
        # a distinct original vertex.
        if sliver_cleanup and faces:
            P, faces = _cleanup_slivers(P, faces, m, min_angle_deg=15.0)
        if not faces:
            log["action"] = "sliver"
            report["loops_skipped_sliver"] = report.get("loops_skipped_sliver", 0) + 1
            continue

        # Zero-volume-shell guard: if this loop is the ONLY boundary of its
        # component, the patch closes that component.  When the closed signed
        # volume is ~0 for its surface area, the component is a flat sheet and
        # the fill would manufacture a zero-thickness double-face shell (the
        # 25 spurious framebaroque parts agy measured).  Leave it open.
        if _fi is not None:
            _comp = int(_lab[_fi])
            if nloops_c[_comp] == 1:
                _tP = P[np.asarray(faces, np.int64)]
                _vp = float(np.einsum("ij,ij->i", _tP[:, 0],
                                      np.cross(_tP[:, 1], _tP[:, 2])).sum() / 6.0)
                _ap = float((0.5 * np.linalg.norm(
                    np.cross(_tP[:, 1] - _tP[:, 0], _tP[:, 2] - _tP[:, 0]),
                    axis=1)).sum())
                _tot = abs(_volc[_comp] + _vp)
                _area = _areac[_comp] + _ap
                if _area > 0 and _tot <= ZERO_VOL_FRAC * _area ** 1.5:
                    report["loops_skipped_flat"] = \
                        report.get("loops_skipped_flat", 0) + 1
                    log["action"] = "zero_vol"
                    continue

        base = len(out_v)

        def gmap(i):
            return int(loop_arr[i]) if i < m else base + (i - m)

        gfaces = [(gmap(a), gmap(b), gmap(c)) for (a, b, c) in faces]

        # skip a lone single-sided triangle (filling it would duplicate a face)
        if m == 3:
            adjs = set()
            for i in range(3):
                g0 = int(loop_arr[i])
                g1 = int(loop_arr[(i + 1) % 3])
                kk = (g0 if g0 < g1 else g1) * np.int64(_env + 1) + \
                    (g1 if g0 < g1 else g0)
                gi = int(np.searchsorted(_fuq, kk))
                if gi < len(_fuq) and _fuq[gi] == kk:
                    s = int(_frs[gi])
                    adjs.update(int(x) for x in _ffo[s:s + int(_frc[gi])])
            if len(adjs) <= 1:
                report["loops_skipped_sheet"] += 1
                log["action"] = "sheet"
                continue

        # guarded commit: never introduce a non-manifold edge
        ce = {}
        for (A, B, C) in gfaces:
            for u, w in ((A, B), (B, C), (C, A)):
                k = (u, w) if u < w else (w, u)
                ce[k] = ce.get(k, 0) + 1
        coll = [(k, gcount.get(k, 0), v) for k, v in ce.items()
                if gcount.get(k, 0) + v > 2]
        if coll:
            report["loops_skipped_nm"] += 1
            report.setdefault("nm_collisions", []).append(
                {"m": int(m), "fan": bool(is_fan),
                 "edges": [[int(x) for x in k] for k, _g, _v in coll[:4]]})
            log["action"] = "nm"
            continue

        if avoid_coplanar and _coplanar_collides(
                P, faces, [], extra_ref,
                overlap_frac=coplanar_frac, cos_tol=coplanar_cos):
            report["loops_skipped_coplanar"] += 1
            log["action"] = "coplanar"
            continue

        for j in range(m, len(P)):
            out_v.append(P[j])
        out_t.extend(gfaces)
        if avoid_coplanar:
            _c, _n, _ml = _face_cent_norm(P, faces)
            extra_ref.append((_c, _n, P[np.asarray(faces, np.int64)],
                              cKDTree(_c)))
        for k, v in ce.items():
            gcount[k] = gcount.get(k, 0) + v
        report["loops_filled"] += 1
        report["patch_faces"] += len(faces)
        log["action"] = "filled"
        if was_seeded:
            report["loops_seeded"] += 1
            report["loops_refined"] += 1
        if collect_quality:
            qa, qb = _tri_angles_aspect(P, faces)
            _q_after_min.append(qa)
            _q_after_asp.append(qb)

    out_v = np.asarray(out_v, dtype=np.float64)
    out_t = (np.asarray(out_t, dtype=np.int64) if out_t
             else np.zeros((0, 3), np.int64))
    report["new_vertices"] = len(out_v) - len(v2)

    if separate_stl and len(out_t):
        out_v, _moved = separate_coincident_stl(out_v, protect_below=len(v2))
        report["separated_stl"] = _moved

    if collect_quality:
        def _pct(vals, q):
            if not len(vals):
                return None
            return float(np.percentile(np.concatenate(vals), q))
        report["quality_before"] = {
            "n": int(sum(len(x) for x in _q_before_min)),
            "min_angle_p5": _pct(_q_before_min, 5),
            "min_angle_p50": _pct(_q_before_min, 50),
            "aspect_p50": _pct(_q_before_asp, 50),
            "aspect_p95": _pct(_q_before_asp, 95)}
        report["quality_after"] = {
            "n": int(sum(len(x) for x in _q_after_min)),
            "min_angle_p5": _pct(_q_after_min, 5),
            "min_angle_p50": _pct(_q_after_min, 50),
            "aspect_p50": _pct(_q_after_asp, 50),
            "aspect_p95": _pct(_q_after_asp, 95)}
    return out_v, out_t, report


def flap_fill(verts, tris, *, refine=True, fair=True, max_loop=1200,
              max_faces=400000, collect_quality=False,
              drop_lone_tris=True, weld_cracks=True, split_nm=True,
              separate_stl=True, bridge_open_chains=False, weld_max_frac=0.02,
              orient="reverse", avoid_coplanar=True, coplanar_frac=0.05,
              coplanar_cos=0.99, sliver_cleanup=False):
    """Fill boundary loops, using the Rust port when the extension has it.

    Dispatches to ``sutura_geom.flap_fill`` (bit-for-bit parity with
    :func:`flap_fill_python`, verified by ``tests/test_flap_rust_parity.py``)
    and falls back to the numpy oracle when the extension is absent, predates
    the binding, or errors.  The signature and the returned ``(verts, tris,
    report)`` are identical for both engines.
    """
    rfn = _load_rust_flap()
    if rfn is not None:
        try:
            rv, rt, rrep = rfn(
                np.ascontiguousarray(verts, dtype=np.float64),
                np.ascontiguousarray(tris, dtype=np.int64),
                refine=refine, fair=fair, max_loop=max_loop,
                max_faces=max_faces, collect_quality=collect_quality,
                drop_lone_tris=drop_lone_tris, weld_cracks=weld_cracks,
                split_nm=split_nm, separate_stl=separate_stl,
                bridge_open_chains=bridge_open_chains,
                weld_max_frac=weld_max_frac, orient=orient,
                avoid_coplanar=avoid_coplanar, coplanar_frac=coplanar_frac,
                coplanar_cos=coplanar_cos, sliver_cleanup=sliver_cleanup)
            return (np.asarray(rv, dtype=np.float64),
                    np.asarray(rt, dtype=np.int64), dict(rrep))
        except Exception:  # noqa: BLE001 - a repair tier never crashes
            pass
    return flap_fill_python(
        verts, tris, refine=refine, fair=fair, max_loop=max_loop,
        max_faces=max_faces, collect_quality=collect_quality,
        drop_lone_tris=drop_lone_tris, weld_cracks=weld_cracks,
        split_nm=split_nm, separate_stl=separate_stl,
        bridge_open_chains=bridge_open_chains, weld_max_frac=weld_max_frac,
        orient=orient, avoid_coplanar=avoid_coplanar,
        coplanar_frac=coplanar_frac, coplanar_cos=coplanar_cos,
        sliver_cleanup=sliver_cleanup)
