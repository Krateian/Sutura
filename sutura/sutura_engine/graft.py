#!/usr/bin/env python3
"""Graft (#13, shell wrap) — standalone morphology-based repair tier.

Graft grows a smooth envelope around the input with the Cast morphology core
(``sutura_geom.morph_close``: generalized-winding signed distance -> dilate ->
true-EDT re-distancing -> erode -> manifold dual contouring), projects the
envelope back onto the healthy part of the original surface, optionally
isotropically re-meshes it and rolls the re-mesh back when it introduces
self-intersections, then applies the reload-safe Stitch (P-WELD) pass and the
X-Ray reload-honest verdict.

Resolution is chosen automatically, finest first: the voxel size is the finest
that fits the grid budget (never finer than the input's median edge), where the
budget scales with the Triage intensity preset and the available system RAM
(``resolve_grid_budget``).  Pass 0 first tries the raw sign field at r = 0 (no
closing, so real gaps stay open); only when it is not reload-watertight and
faithful does the closing radius ladder run, starting at the smallest useful
radius (≈1.5 voxel) and growing ×1.6 while the result is not reload-watertight.

The module is intentionally independent of ``methods.py``/the method registry:
it imports only numpy, scipy, trimesh, ``sutura_geom`` and a handful of pure
helpers from ``repair.py``/``defects.py``, so it can be moved into the engine
package without rework.  It is a *geometry-inventing* tier (it closes holes and
openings by adding material), hence ``invents_geometry=True``, the one-sided
plus two-sided-on-healthy Hausdorff guards and the detail-loss warning.

Pure API::

    shell_wrap(verts, tris, *, r=None, voxel=None, box=None, local=None,
               remesh=True, fill_cavities=False, max_tries=3,
               target_edge=None, ml=None, si_check=True, guard=True,
               grid_budget=None, detail_tol=None, intensity=None,
               sign_field=None)
        -> (verts_out, tris_out, report)

No file is written.  ``report['per_vertex_deviation']`` carries the per-result-
vertex distance to the original surface for the heatmap view.
"""
from __future__ import annotations

import os
import sys
import time
from collections import defaultdict

import numpy as np
import trimesh

import sutura_geom
from defects import detect as detect_defects
from repair import (
    _damaged_region,
    _referenced_only,
    _separate_weld_collisions,
    reload_strict_holes_nm,
)

METHOD_NUMBER = 13
DISPLAY_NAME = "Graft"
DISPLAY_NAME_FULL = "Graft (shell wrap)"

# --- automatic resolution --------------------------------------------------
# The auto voxel is coarsened only as far as the grid budget requires.  The
# budget scales with the intensity preset and the available system RAM, capped
# hard so a single morphology grid can never exhaust memory.  GRID_BUDGET is
# the conservative floor (the historical fixed value); the peak Rust usage is
# roughly GRID_BUDGET_BYTES_PER_CELL bytes per cell.
GRID_BUDGET = 1_500_000          # minimum / Quick target
GRID_BUDGET_HARD_MAX = 25_000_000  # never exceed, regardless of RAM/intensity
GRID_BUDGET_BYTES_PER_CELL = 56  # empirical peak bytes per grid cell
GRID_BUDGET_RAM_RESERVE = 0.35   # fraction of available RAM the grid may use
GRID_BUDGET_BY_INTENSITY = {
    # Balanced keeps the historical 1.5M budget: on the 40-mesh corpus the
    # 5M target changed no watertight outcome while roughly doubling Graft
    # time (the sign-field/closing grid is ~3x larger). Thorough/Extreme
    # still scale up for users who opt into more work.
    'quick': 1_500_000,
    'balanced': 1_500_000,
    'thorough': 10_000_000,
    'extreme': 25_000_000,
}
MIN_GRID_DIV = 32                # never coarser than bbox/32
MAX_GRID_DIV = 512               # never finer than bbox/512
BUDGET_PAD = 8                   # grid pad assumed while budgeting

# --- closing radius ladder (finest first) ----------------------------------
R_START_VOXELS = 1.5             # start r at ~1.5 voxel
R_STEP = 1.6                     # grow x1.6 while not reload-watertight
R_MAX_DIAG_FRACTION = 0.02       # hard upper cap: 2 % of the bbox diagonal
MAX_TRIES = 3

# --- wrap / projection -----------------------------------------------------
HEALTHY_DIST_FACTOR = 1.5        # project when distance < 1.5 r ...
HEALTHY_NORMAL_DOT = 0.5         # ... and the normals agree (dot > 0.5)

# --- local window ----------------------------------------------------------
LOCAL_MAX_BBOX_FRACTION = 0.30   # damaged bbox below 30 % of the model bbox
LOCAL_WINDOW_MARGIN = 3.0        # expand the damaged bbox by 3 r

# --- guards / detail loss --------------------------------------------------
HEALTHY_HAUSDORFF_MAX = 0.0025    # two-sided-on-healthy, rel. to bbox diagonal
ONE_SIDED_HAUSDORFF_MAX = 0.005   # input -> output, rel. to bbox diagonal
HAUSDORFF_SAMPLES = 4000
DETAIL_TOL_MM = 0.1              # default tolerance: 0.1 mm
DETAIL_TOL_FRACTION = 0.002      # ... or 0.2 % of the bbox diagonal
DETAIL_WARN_AREA_FRACTION = 0.02  # warn when >2 % of the area moved beyond tol

# --- remesh / SI guards ----------------------------------------------------
REMESH_MAX_FACES = 200_000
REMESH_MAX_PASSES = 6
SI_MAX_FACES = int(getattr(sutura_geom, "SI_MAX_FACES", 150_000))
# The exact SI classifier is super-linear on large projected envelopes; above
# this the SI rollback is skipped (reported as None) to keep the tier fast.
SHELL_WRAP_SI_MAX_FACES = 20_000
SI_ROLLBACK = True
HYBRID_MAX_HOLE = 200_000       # pymeshlab close_holes budget for the hybrid
REFINE_MAX_FACES = 300_000      # do not adopt a refinement past this size
REFINE_FACE_FACTOR = 4          # ... nor past this multiple of the input faces

# --- ray-stabbing disambiguation (opt-in, experimental) --------------------
# The Cast morphology core builds the signed field from the generalized winding
# number, which is correct for consistently oriented closed shells but can pick
# the wrong side in the winding-ambiguous band (0.3-0.7) on overlapping or
# inconsistently oriented soups.  ``raystab=True`` (or SUTURA_RAYSTAB=1) votes
# inside/outside with a small set of rays to override the sign there.  It is
# OFF by default and byte-identical to the winding-only path when off.
_RAYSTAB_SUPPORTED = hasattr(sutura_geom, 'raystab_grid')
_RAYSTAB_ENV_TRUE = {'1', 'true', 'yes', 'on'}


def _resolve_raystab(raystab):
    """Resolve the opt-in from the parameter, then SUTURA_RAYSTAB, then off.

    Returns False (a no-op) when the installed extension predates the feature.
    """
    if raystab is None:
        raystab = os.environ.get('SUTURA_RAYSTAB', '').strip().lower() in _RAYSTAB_ENV_TRUE
    return bool(raystab) and _RAYSTAB_SUPPORTED


# --- sign-field Pass 0 (r = 0, no closing; DEFAULT OFF, opt-in) -------------
# When enabled, Graft tries the raw generalized-winding sign field (with the
# ray-stab vote when enabled) at r = 0 before the closing ladder.  That keeps
# genuine gaps open instead of bridging them; the strict X-Ray reload check and
# the healthy-region detail gate decide whether it is adopted, otherwise the
# closing ladder runs unchanged.  It is OFF by default: measured on
# framebaroque it fails both gates (2 holes / 105 non-manifold edges and 1.29 %
# detail loss vs the 0.25 % gate) and wastes ~326 s.  Enable with
# sign_field=True / SUTURA_GRAFT_SIGN_FIELD=1.
_SIGN_FIELD_SUPPORTED = bool(getattr(sutura_geom, 'SIGN_FIELD_SUPPORTED', False))
_SIGN_FIELD_ENV_TRUE = {'1', 'true', 'yes', 'on'}


def _resolve_sign_field(sign_field):
    """Resolve Pass 0 from the parameter, then SUTURA_GRAFT_SIGN_FIELD, then the
    default (off).  Returns False when the extension cannot run r = 0."""
    if sign_field is None:
        env = os.environ.get('SUTURA_GRAFT_SIGN_FIELD', '').strip().lower()
        if env in _SIGN_FIELD_ENV_TRUE:
            sign_field = True
        else:
            sign_field = False
    return bool(sign_field) and _SIGN_FIELD_SUPPORTED


def _available_memory_bytes():
    """Best-effort available RAM in bytes (stdlib only).

    Order: psutil (if present) -> ``os.sysconf`` (Linux) -> ``sysctl hw.memsize``
    halved on macOS -> a conservative 4 GB fallback.  Never raises.
    """
    try:
        import psutil  # type: ignore
        return int(psutil.virtual_memory().available)
    except Exception:  # noqa: BLE001
        pass
    try:
        pages = os.sysconf('SC_AVPHYS_PAGES')
        size = os.sysconf('SC_PAGE_SIZE')
        if pages > 0 and size > 0:
            return int(pages) * int(size)
    except Exception:  # noqa: BLE001
        pass
    if sys.platform == 'darwin':
        try:
            import subprocess
            out = subprocess.check_output(
                ['sysctl', '-n', 'hw.memsize']).decode().strip()
            return int(int(out) * 0.5)
        except Exception:  # noqa: BLE001
            pass
    return 4 * 1024 * 1024 * 1024


def resolve_grid_budget(intensity=None, available_bytes=None):
    """Resolve the morphology grid budget (cells) from the intensity preset and
    the available RAM, clamped to the floor and the hard cap.

    ``intensity`` is the preset name (or a custom profile's base); unknown/None
    resolves to the balanced target.  The RAM safety limit is
    ``available * reserve / bytes_per_cell`` and is an absolute ceiling: it can
    clamp the budget BELOW ``GRID_BUDGET`` down to the practical grid floor
    ``MIN_GRID_DIV ** 3`` (a low-memory host must never allocate the nominal
    84 MB).  Never exceeds ``GRID_BUDGET_HARD_MAX``.
    """
    target = GRID_BUDGET_BY_INTENSITY.get(intensity,
                                          GRID_BUDGET_BY_INTENSITY['balanced'])
    if available_bytes is None:
        available_bytes = _available_memory_bytes()
    safe = int((max(0, int(available_bytes)) * GRID_BUDGET_RAM_RESERVE)
               / GRID_BUDGET_BYTES_PER_CELL)
    nominal = max(GRID_BUDGET, min(int(target), GRID_BUDGET_HARD_MAX))
    ram_floor = min(GRID_BUDGET, MIN_GRID_DIV ** 3)
    return int(max(ram_floor, min(nominal, safe)))


# --------------------------------------------------------------------------- #
# small geometry helpers
# --------------------------------------------------------------------------- #
def _as_f64(v):
    return np.ascontiguousarray(np.asarray(v, dtype=np.float64))


def _as_i64(t):
    return np.ascontiguousarray(np.asarray(t, dtype=np.int64))


def _bbox(v):
    if len(v) == 0:
        return np.zeros(3), np.zeros(3)
    return v.min(axis=0), v.max(axis=0)


def _diag(v):
    if len(v) == 0:
        return 0.0
    lo, hi = _bbox(v)
    return float(np.linalg.norm(hi - lo))


def _face_normals(v, t):
    tri = v[t]
    n = np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0])
    ln = np.linalg.norm(n, axis=1)
    ln[ln == 0.0] = 1.0
    return n / ln[:, None]


def _vertex_normals(v, t):
    fn = _face_normals(v, t)
    n = np.zeros_like(v)
    for k in range(3):
        np.add.at(n, t[:, k], fn)
    ln = np.linalg.norm(n, axis=1)
    ln[ln == 0.0] = 1.0
    return n / ln[:, None]


def _unique_edges(t):
    he = np.concatenate([t[:, [0, 1]], t[:, [1, 2]], t[:, [2, 0]]], axis=0)
    key = np.sort(he, axis=1)
    uniq, inv = np.unique(key, axis=0, return_inverse=True)
    return uniq, np.asarray(inv).reshape(-1)


def _edge_use_counts(t):
    he = np.concatenate([t[:, [0, 1]], t[:, [1, 2]], t[:, [2, 0]]], axis=0)
    key = np.sort(he, axis=1)
    uniq, counts = np.unique(key, axis=0, return_counts=True)
    return uniq, np.asarray(counts)


def _edge_lengths(v, edges):
    if len(edges) == 0:
        return np.zeros(0)
    return np.linalg.norm(v[edges[:, 1]] - v[edges[:, 0]], axis=1)


def _median_edge_length(v, t):
    if len(t) == 0:
        return 0.0
    edges, _ = _unique_edges(t)
    L = _edge_lengths(v, edges)
    return float(np.median(L)) if len(L) else 0.0


def _sample_surface(v, t, n, rng):
    if len(t) == 0 or n <= 0:
        return np.zeros((0, 3))
    tri = v[t]
    area = 0.5 * np.linalg.norm(
        np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0]), axis=1)
    tot = float(area.sum())
    if tot <= 0.0:
        return np.zeros((0, 3))
    p = area / tot
    k = int(min(n, max(len(t), 1) * 8))
    idx = rng.choice(len(t), size=k, p=p)
    u = rng.random(k)
    w = rng.random(k)
    flip = u + w > 1.0
    u[flip] = 1.0 - u[flip]
    w[flip] = 1.0 - w[flip]
    a = tri[idx, 0]
    b = tri[idx, 1]
    c = tri[idx, 2]
    return a + u[:, None] * (b - a) + w[:, None] * (c - a)


def _one_sided_hausdorff(va, ta, vb, tb, diag, n=HAUSDORFF_SAMPLES, rng=None):
    rng = rng if rng is not None else np.random.default_rng(12345)
    if len(ta) == 0 or len(tb) == 0 or diag <= 0.0:
        return None, None
    pts = _sample_surface(va, ta, n, rng)
    if len(pts) == 0:
        return None, None
    _q, d, _f = sutura_geom.closest_points(vb, tb, pts)
    d = np.asarray(d)
    return float(d.max() / diag), float(d.mean() / diag)


def _behind_on_healthy(v0, t0, region_mask, wv, wt, diag, r, rng):
    """Result -> original deviation, restricted to result samples whose nearest
    original face is in the healthy region (samples over a deliberately closed
    opening are excluded, so the metric does not penalise the repair itself)."""
    if len(t0) == 0 or len(wt) == 0 or diag <= 0.0:
        return None
    pts = _sample_surface(wv, wt, HAUSDORFF_SAMPLES, rng)
    if len(pts) == 0:
        return None
    _q, d, fid = sutura_geom.closest_points(v0, t0, pts)
    d = np.asarray(d)
    fid = np.asarray(fid)
    if region_mask is not None and len(region_mask) == len(t0):
        keep = (fid >= 0) & (~region_mask[np.clip(fid, 0, len(region_mask) - 1)])
    else:
        keep = fid >= 0
    keep &= d < HEALTHY_DIST_FACTOR * r
    if not keep.any():
        return None
    return float(d[keep].max() / diag)


# --------------------------------------------------------------------------- #
# analysis
# --------------------------------------------------------------------------- #
def _auto_voxel(v, t, diag, grid_budget):
    """Voxel scale for the envelope: the input's median edge length, coarsened
    only as far as the grid budget requires.  This keeps the envelope
    resolution — and therefore the output face count — in the same order as
    the input; the envelope is refined (split) to this target again before
    projecting.  A dense scan whose median-edge grid exceeds the budget is
    coarsened and then handled by the verbatim hybrid."""
    if len(v) == 0:
        return 1.0
    ext = v.max(axis=0) - v.min(axis=0)
    maxext = max(float(ext.max()), 1e-12)
    voxel = _median_edge_length(v, t)
    if not (voxel > 0.0):
        voxel = maxext / 96.0
    voxel = max(voxel, maxext / MAX_GRID_DIV)
    if grid_budget is None:
        return float(voxel)
    for _ in range(120):
        dims = np.ceil(np.maximum(ext, 0.0) / voxel) + 1 + 2 * BUDGET_PAD
        dims = np.maximum(dims, 3)
        if float(np.prod(dims)) <= grid_budget:
            break
        voxel *= 1.15
    return float(voxel)


def _gap_estimate(verts, tris):
    """Largest hole/gap size estimate (sets only the r upper cap) plus the
    damaged-region info.  Returns ``(gap, info, region_mask)``."""
    d = detect_defects(verts, tris, with_indices=True)
    holes = d["holes"]
    nm = d["non_manifold"]
    gap = 0.0
    for h in holes:
        gap = max(gap, float(h.get("diameter") or 0.0))
    region_mask, _n_regions = _damaged_region(tris)
    region_diag = 0.0
    if region_mask.any():
        rv = np.unique(tris[region_mask].reshape(-1))
        region_diag = _diag(verts[rv])
    if gap <= 0.0:
        gap = region_diag
    info = {
        "holes": len(holes),
        "non_manifold_regions": len(nm),
        "gap": float(gap),
        "damaged_faces": int(region_mask.sum()),
        "damaged_bbox_diag": float(region_diag),
        "damaged_bbox_fraction": 0.0,
    }
    return gap, info, region_mask


def _damaged_bbox(verts, tris, region_mask):
    if region_mask is None or not region_mask.any():
        return None
    rv = np.unique(tris[region_mask].reshape(-1))
    return _bbox(verts[rv])


def _r_ladder(gap, voxel, diag, max_tries):
    """Finest-first ladder: start at ~1.5 voxel, grow x1.6 while not
    watertight, capped by the hole estimate / 2 % diagonal."""
    r0 = R_START_VOXELS * voxel
    cap = R_MAX_DIAG_FRACTION * diag if diag > 0.0 else r0
    if gap > 0.0:
        cap = min(cap, 0.5 * gap) if cap > 0 else 0.5 * gap
    cap = max(cap, r0)
    out = []
    cur = r0
    for _ in range(max(1, int(max_tries))):
        out.append(min(cur, cap))
        if cur >= cap:
            break
        cur *= R_STEP
    return out


# --------------------------------------------------------------------------- #
# wrap / projection
# --------------------------------------------------------------------------- #
def _project_envelope(ev, et, ov, ot, r):
    """Move envelope vertices onto the original surface where healthy.

    Healthy = closest original point closer than ``1.5 r`` AND the envelope
    vertex normal agrees with the original face normal (dot > 0.5)."""
    if len(ev) == 0 or len(ot) == 0:
        return ev, np.zeros(len(ev), dtype=bool)
    q, d, fid = sutura_geom.closest_points(ov, ot, ev)
    q = np.asarray(q)
    d = np.asarray(d)
    fid = np.asarray(fid)
    vn = _vertex_normals(ev, et)
    fn = _face_normals(ov, ot)
    valid = fid >= 0
    safe_fid = np.clip(fid, 0, max(len(ot) - 1, 0))
    on = fn[safe_fid]
    healthy = valid & (d < HEALTHY_DIST_FACTOR * r)
    healthy &= np.einsum("ij,ij->i", vn, on) > HEALTHY_NORMAL_DOT
    pv = ev.copy()
    pv[healthy] = q[healthy]
    return pv, healthy


# --------------------------------------------------------------------------- #
# isotropic re-mesh (numpy; no pymeshlab)
# --------------------------------------------------------------------------- #
def _split_long_edges(v, t, target):
    thr = (4.0 / 3.0) * target
    for _ in range(REMESH_MAX_PASSES):
        edges, inv = _unique_edges(t)
        L = _edge_lengths(v, edges)
        long = L > thr
        if not long.any():
            break
        long_ids = np.nonzero(long)[0]
        mid_id = np.full(len(edges), -1, dtype=np.int64)
        mid = 0.5 * (v[edges[long_ids, 0]] + v[edges[long_ids, 1]])
        mid_id[long_ids] = np.arange(len(v), len(v) + len(long_ids))
        v = np.vstack([v, mid])

        F = len(t)
        ei = inv.reshape(3, F).T
        m01 = mid_id[ei[:, 0]]
        m12 = mid_id[ei[:, 1]]
        m20 = mid_id[ei[:, 2]]
        a, b, c = t[:, 0], t[:, 1], t[:, 2]
        s = (m01 >= 0).astype(np.int8) + (m12 >= 0) + (m20 >= 0)
        pieces = [t[s == 0]]

        m = s == 1
        if m.any():
            A, B, C = a[m], b[m], c[m]
            mm01, mm12, mm20 = mid_id[ei[m, 0]], mid_id[ei[m, 1]], mid_id[ei[m, 2]]
            sel = mm01 >= 0
            if sel.any():
                pieces.append(np.column_stack([A[sel], mm01[sel], C[sel]]))
                pieces.append(np.column_stack([mm01[sel], B[sel], C[sel]]))
            sel = mm12 >= 0
            if sel.any():
                pieces.append(np.column_stack([B[sel], mm12[sel], A[sel]]))
                pieces.append(np.column_stack([mm12[sel], C[sel], A[sel]]))
            sel = mm20 >= 0
            if sel.any():
                pieces.append(np.column_stack([C[sel], mm20[sel], B[sel]]))
                pieces.append(np.column_stack([mm20[sel], A[sel], B[sel]]))

        m = s == 2
        if m.any():
            A, B, C = a[m], b[m], c[m]
            mm01, mm12, mm20 = mid_id[ei[m, 0]], mid_id[ei[m, 1]], mid_id[ei[m, 2]]
            sel = mm20 < 0
            if sel.any():
                pieces.append(np.column_stack([A[sel], mm01[sel], C[sel]]))
                pieces.append(np.column_stack([mm01[sel], B[sel], mm12[sel]]))
                pieces.append(np.column_stack([mm12[sel], C[sel], A[sel]]))
            sel = mm01 < 0
            if sel.any():
                pieces.append(np.column_stack([B[sel], mm12[sel], A[sel]]))
                pieces.append(np.column_stack([mm12[sel], C[sel], mm20[sel]]))
                pieces.append(np.column_stack([mm20[sel], A[sel], B[sel]]))
            sel = mm12 < 0
            if sel.any():
                pieces.append(np.column_stack([C[sel], mm20[sel], B[sel]]))
                pieces.append(np.column_stack([mm20[sel], A[sel], mm01[sel]]))
                pieces.append(np.column_stack([mm01[sel], B[sel], C[sel]]))

        m = s == 3
        if m.any():
            A, B, C = a[m], b[m], c[m]
            mm01, mm12, mm20 = mid_id[ei[m, 0]], mid_id[ei[m, 1]], mid_id[ei[m, 2]]
            pieces.append(np.column_stack([A, mm01, mm20]))
            pieces.append(np.column_stack([mm01, B, mm12]))
            pieces.append(np.column_stack([mm20, mm12, C]))
            pieces.append(np.column_stack([mm01, mm12, mm20]))

        t = np.vstack([p for p in pieces if len(p)])
    return v, t


def _collapse_short_edges(v, t, target):
    """Collapse edges shorter than 4/5 target in matching passes (manifold link
    condition).  The caller adopts the result only when it is not worse."""
    thr = (4.0 / 5.0) * target
    for _ in range(REMESH_MAX_PASSES):
        edges, _inv = _unique_edges(t)
        if len(edges) == 0:
            break
        L = _edge_lengths(v, edges)
        cand = np.nonzero(L < thr)[0]
        if len(cand) == 0:
            break
        adj = defaultdict(set)
        for a, b in edges:
            adj[int(a)].add(int(b))
            adj[int(b)].add(int(a))
        parent = np.arange(len(v), dtype=np.int64)

        def find(x):
            while parent[x] != x:
                parent[x] = parent[parent[x]]
                x = parent[x]
            return x

        merged = np.zeros(len(v), dtype=bool)
        any_merge = False
        for a, b in edges[cand]:
            a, b = int(a), int(b)
            if merged[a] or merged[b]:
                continue
            if len(adj[a] & adj[b]) != 2:
                continue
            merged[a] = merged[b] = True
            ra, rb = find(a), find(b)
            if ra != rb:
                parent[rb] = ra
            any_merge = True
        if not any_merge:
            break

        root = np.array([find(i) for i in range(len(v))])
        rep_of_root = {}
        acc = {}
        cnt = {}
        for i in range(len(v)):
            r = int(root[i])
            if r not in rep_of_root:
                rep_of_root[r] = i
                acc[r] = np.zeros(3)
                cnt[r] = 0
            acc[r] = acc[r] + v[i]
            cnt[r] += 1
        roots = list(rep_of_root)
        root_ids = {r: k for k, r in enumerate(roots)}
        new_v = np.array([acc[r] / cnt[r] for r in roots])
        remap = np.array([root_ids[int(root[i])] for i in range(len(v))], dtype=np.int64)
        nt = remap[t]
        good = (nt[:, 0] != nt[:, 1]) & (nt[:, 1] != nt[:, 2]) & (nt[:, 0] != nt[:, 2])
        nt = nt[good]
        if len(nt) == 0:
            break
        key = np.sort(nt, axis=1)
        _u, first = np.unique(key, axis=0, return_index=True)
        nt = nt[np.sort(first)]
        used = np.unique(nt)
        r2 = np.full(len(new_v), -1, dtype=np.int64)
        r2[used] = np.arange(len(used))
        v, t = new_v[used], r2[nt]
    return v, t


def _smooth_tangential(v, t, iterations=2):
    if len(v) == 0 or len(t) == 0:
        return v
    a = np.concatenate([t[:, 0], t[:, 1], t[:, 2]])
    b = np.concatenate([t[:, 1], t[:, 2], t[:, 0]])
    src = np.concatenate([a, b])
    dst = np.concatenate([b, a])
    deg = np.bincount(src, minlength=len(v)).astype(np.float64)
    boundary = np.zeros(len(v), dtype=bool)
    edges, counts = _edge_use_counts(t)
    if len(edges):
        bverts = edges[counts == 1].reshape(-1)
        if len(bverts):
            boundary[bverts] = True
    free = (~boundary) & (deg > 0)
    if not free.any():
        return v
    out = v.copy()
    for _ in range(iterations):
        vn = _vertex_normals(out, t)
        acc = np.zeros_like(out)
        np.add.at(acc, src, out[dst])
        cen = acc[free] / deg[free, None]
        disp = cen - out[free]
        disp -= (np.einsum("ij,ij->i", disp, vn[free]))[:, None] * vn[free]
        out[free] += 0.5 * disp
    return out


def _isotropic_remesh(v, t, target):
    if target <= 0.0 or len(t) == 0:
        return v, t
    try:
        v, t = _split_long_edges(v, t, target)
        v, t = _collapse_short_edges(v, t, target)
        v = _smooth_tangential(v, t, iterations=2)
    except Exception:  # noqa: BLE001 - re-mesh is best-effort
        return v, t
    return v, t


# --------------------------------------------------------------------------- #
# self-intersection rollback
# --------------------------------------------------------------------------- #
def _si_count(v, t):
    limit = min(SI_MAX_FACES, SHELL_WRAP_SI_MAX_FACES)
    if len(t) == 0 or len(t) > limit:
        return None
    try:
        _mask, count = sutura_geom.self_intersecting_faces(v, t)
        return int(count)
    except Exception:  # noqa: BLE001
        return None


def _si_rollback(v_remesh, t_remesh, v_wrap, t_wrap, si_before):
    if not SI_ROLLBACK:
        return v_remesh, t_remesh, _si_count(v_remesh, t_remesh), False
    si_after = _si_count(v_remesh, t_remesh)
    if si_after is None:
        return v_remesh, t_remesh, None, False
    if si_before is not None and si_after > si_before:
        return v_wrap, t_wrap, si_before, True
    return v_remesh, t_remesh, si_after, False


# --------------------------------------------------------------------------- #
# Stitch (P-WELD) + X-Ray (honest verdict)
# --------------------------------------------------------------------------- #
def _p_weld(v, t, ml=None):
    """Reload-safe Stitch pass: the pymeshlab P-WELD when ``ml`` is supplied,
    otherwise the pure split-collisions branch."""
    v = np.asarray(v, dtype=np.float32)
    t = np.asarray(t, dtype=np.int64)
    h, nm = reload_strict_holes_nm(v, t)
    if h == 0 and nm == 0:
        return v, t, None
    if ml is not None:
        try:
            from repair import p_weld_final
            return p_weld_final(ml, v, t)
        except Exception:  # noqa: BLE001
            pass
    try:
        cv, ct = _separate_weld_collisions(v, t)
        ch, cnm = reload_strict_holes_nm(cv, ct)
        if ch == 0 and cnm == 0:
            return cv, ct, {"applied": True, "method": "split-collisions"}
    except Exception:  # noqa: BLE001
        pass
    return v, t, {"applied": False}


def _cap_boundary_loops(v, t):
    """Close every remaining boundary component with a centroid fan.

    Each boundary half-edge `(a, b)` gets a new triangle `(b, a, c)` sharing a
    per-component centroid `c`, so the boundary edge becomes manifold and the
    component is sealed regardless of the loop shape.  Pure numpy."""
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    if len(t) == 0:
        return v, t
    he = np.concatenate([t[:, [0, 1]], t[:, [1, 2]], t[:, [2, 0]]]).tolist()
    eset = set((int(a), int(b)) for a, b in he)
    bnd = [(int(a), int(b)) for a, b in he if (int(b), int(a)) not in eset]
    if not bnd:
        return v, t
    par = {}

    def find(x):
        par.setdefault(x, x)
        while par[x] != x:
            par[x] = par[par[x]]
            x = par[x]
        return x

    for a, b in bnd:
        ra, rb = find(a), find(b)
        if ra != rb:
            par[ra] = rb
    comps = {}
    for a, b in bnd:
        comps.setdefault(find(a), []).append((a, b))
    addv = []
    addt = []
    for edges in comps.values():
        cvts = np.unique(np.asarray(edges).reshape(-1))
        addv.append(v[cvts].mean(axis=0))
        cidx = len(v) + len(addv) - 1
        for a, b in edges:
            addt.append((b, a, cidx))
    nv = np.vstack([v, np.asarray(addv, dtype=np.float64)])
    nt = np.vstack([t, np.asarray(addt, dtype=np.int64)])
    nt = nt[(nt[:, 0] != nt[:, 1]) & (nt[:, 1] != nt[:, 2])
            & (nt[:, 0] != nt[:, 2])]
    key = np.sort(nt, axis=1)
    _u, first = np.unique(key, axis=0, return_index=True)
    nt = nt[np.sort(first)]
    used = np.unique(nt)
    remap = np.full(len(nv), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return nv[used], remap[nt]


def _hybrid_close(v0, t0, region_mask, ml):
    """Verbatim hybrid: drop the damaged-region faces and close the openings
    with the pymeshlab repair path plus a numpy boundary cap, keeping every
    healthy original triangle unchanged.  Returns ``(v, t)`` only when the
    result is reload-watertight, otherwise ``None``."""
    if ml is None or region_mask is None or not (~region_mask).any():
        return None
    bv, bt = _referenced_only(v0, t0[~region_mask])
    if len(bt) == 0:
        return None
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(bv, np.float64),
                            face_matrix=np.asarray(bt, np.int32)))
        for _ in range(2):
            for name, kw in (
                    ('meshing_repair_non_manifold_edges', {}),
                    ('meshing_repair_non_manifold_vertices', {}),
                    ('meshing_remove_duplicate_faces', {}),
                    ('meshing_close_holes', {'maxholesize': HYBRID_MAX_HOLE,
                                             'refinehole': True}),
                    ('meshing_remove_unreferenced_vertices', {})):
                try:
                    ms.apply_filter(name, **kw)
                except Exception:  # noqa: BLE001
                    pass
        m = ms.current_mesh()
        ov = np.asarray(m.vertex_matrix(), np.float64)
        ot = np.asarray(m.face_matrix(), np.int64)
        # Seal any residual boundary components, then reload-safe Stitch.
        ov, ot = _cap_boundary_loops(ov, ot)
        ov, ot, _ = _p_weld(ov, ot, None)
        h, nm = reload_strict_holes_nm(ov, ot)
        if h == 0 and nm == 0:
            return ov, ot
    except Exception:  # noqa: BLE001
        pass
    return None


# --------------------------------------------------------------------------- #
# local-window stitch (boolean union with the untouched rest)
# --------------------------------------------------------------------------- #
def _stitch_local(patch_v, patch_t, orig_v, orig_t):
    try:
        patch = trimesh.Trimesh(vertices=np.asarray(patch_v, np.float64),
                                faces=np.asarray(patch_t, np.int64),
                                process=True)
        rest = trimesh.Trimesh(vertices=np.asarray(orig_v, np.float64),
                               faces=np.asarray(orig_t, np.int64),
                               process=False)
        if not patch.is_watertight:
            return patch_v, patch_t, False
        out = trimesh.boolean.union([patch, rest], engine="manifold")
        if out is None or len(out.faces) == 0:
            return patch_v, patch_t, False
        return (np.asarray(out.vertices, np.float64),
                np.asarray(out.faces, np.int64), True)
    except Exception:  # noqa: BLE001
        return patch_v, patch_t, False


# --------------------------------------------------------------------------- #
# detail loss
# --------------------------------------------------------------------------- #
def _detail_loss(v0, t0, res_v, res_t, region_mask, diag, detail_tol):
    """Deviation original->result on healthy regions, plus the per-result-vertex
    deviation to the original surface (for the heatmap)."""
    healthy_faces = (~region_mask if region_mask is not None
                     else np.ones(len(t0), dtype=bool))
    if not healthy_faces.any():
        healthy_faces = np.ones(len(t0), dtype=bool)
    hv, ht = _referenced_only(v0, t0[healthy_faces])
    _q, d, _f = sutura_geom.closest_points(res_v, res_t, hv)
    d = np.asarray(d)
    tri = hv[ht]
    area = 0.5 * np.linalg.norm(
        np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0]), axis=1)
    tol = float(detail_tol if detail_tol is not None
                else max(DETAIL_TOL_MM, DETAIL_TOL_FRACTION * diag))
    face_dev = d[ht].max(axis=1)
    total = float(area.sum()) or 1.0
    moved = float(area[face_dev > tol].sum() / total)
    _q2, d2, _f2 = sutura_geom.closest_points(v0, t0, res_v)
    return {
        "max": float(d.max()) if len(d) else 0.0,
        "mean": float(d.mean()) if len(d) else 0.0,
        "tolerance": tol,
        "area_moved_fraction": moved,
        "rel_max": float(d.max() / diag) if diag > 0 and len(d) else 0.0,
        "per_vertex_deviation": np.asarray(d2, dtype=np.float64),
    }


def _warnings_for(r_used, detail, diag):
    out = []
    if detail is None:
        return out
    if (detail["area_moved_fraction"] > DETAIL_WARN_AREA_FRACTION
            or detail["max"] > 4.0 * detail["tolerance"]):
        pct = 100.0 * detail["area_moved_fraction"]
        out.append({
            "code": "detail_loss",
            "message_en": (
                f"Graft used a {r_used:.3f} mm envelope: {pct:.1f}% of the "
                f"surface moved more than {detail['tolerance']:.3f} mm "
                f"(fine surface texture smoothed)."),
            "message_tr": (
                f"Graft {r_used:.3f} mm zarf kullandı: yüzeyin %{pct:.1f}'i "
                f"{detail['tolerance']:.3f} mm'den fazla hareket etti (ince "
                f"yüzey dokusu yumuşatıldı)."),
        })
    return out


# --------------------------------------------------------------------------- #
# main entry point
# --------------------------------------------------------------------------- #
def shell_wrap(verts, tris, *, r=None, voxel=None, box=None, local=None,
               remesh=True, fill_cavities=False, max_tries=MAX_TRIES,
               target_edge=None, ml=None, si_check=True, guard=True,
               grid_budget=None, detail_tol=None, intensity=None,
               sign_field=None,
               raystab=None, raystab_dirs=None, raystab_seed=0,
               raystab_parity=False):
    """Graft (shell wrap) repair.

    ``r`` fixes the closing radius (otherwise the sign-field Pass 0 + the
    finest-first closing ladder is used); ``voxel``/``box`` are forwarded to
    ``sutura_geom.morph_close``; ``local=True`` forces the local-window path
    (``None`` decides from the damaged-region bbox); ``remesh`` toggles the
    isotropic re-mesh; ``ml`` is an optional live pymeshlab module for the full
    Stitch (P-WELD) pass.

    ``grid_budget`` caps the grid cells; ``None`` resolves it from ``intensity``
    (the Triage preset name) and the available system RAM (see
    ``resolve_grid_budget``).  ``sign_field`` (or ``SUTURA_GRAFT_SIGN_FIELD``)
    enables sign-field Pass 0; it is OFF by default (opt-in) and falls back to
    the closing ladder whenever the r = 0 result is not watertight and faithful.

    ``raystab`` (or the ``SUTURA_RAYSTAB`` env var) enables ray-stabbing
    disambiguation of the generalized-winding sign in the ambiguous band; it is
    OFF by default, so the output is byte-identical to the winding-only path.

    Returns ``(verts, tris, report)``; the report always carries
    ``method=13``, ``display_name='Graft'`` and ``invents_geometry=True``.
    """
    t_start = time.perf_counter()
    v0 = _as_f64(verts)
    t0 = _as_i64(tris)
    if grid_budget is None:
        grid_budget = resolve_grid_budget(intensity)
    grid_budget = int(grid_budget)
    report = {
        "method": METHOD_NUMBER,
        "display_name": DISPLAY_NAME,
        "display_name_full": DISPLAY_NAME_FULL,
        "invents_geometry": True,
        "ok": False,
        "category": "error",
        "tries": 0,
        "r_used": None,
        "mode": "whole",
        "r_ladder": [],
        "voxel": None,
        "grid_budget": int(grid_budget) if grid_budget else None,
        "intensity": intensity,
        "sign_field": None,
        "projected_fraction": 0.0,
        "remeshed": False,
        "si_before": None,
        "si_after": None,
        "si_rolled_back": False,
        "detail_max_mm": None,
        "detail_mean_mm": None,
        "detail_tolerance_mm": None,
        "detail_area_moved": None,
        "warnings": [],
        "per_vertex_deviation": None,
        "fidelity_ok": None,
        "hausdorff_one_sided": None,
        "hausdorff_healthy": None,
        "hausdorff_healthy_behind": None,
        "holes": None,
        "non_manifold": None,
        "faces_before": int(len(t0)),
        "faces_after": int(len(t0)),
        "seconds": 0.0,
    }
    if len(v0) == 0 or len(t0) == 0:
        report["error"] = "empty mesh"
        report["seconds"] = round(time.perf_counter() - t_start, 3)
        return v0, t0, report

    use_raystab = _resolve_raystab(raystab)
    report["raystab"] = None
    raystab_kwargs = {}
    if use_raystab:
        raystab_kwargs = {
            "raystab": True,
            "raystab_dirs": raystab_dirs,
            "raystab_seed": int(raystab_seed),
            "raystab_parity": bool(raystab_parity),
        }

    use_sign_field = _resolve_sign_field(sign_field)
    report["sign_field"] = use_sign_field

    diag = _diag(v0)
    ext = v0.max(axis=0) - v0.min(axis=0)
    if voxel is None:
        voxel = _auto_voxel(v0, t0, diag, grid_budget)
    voxel = float(voxel)
    report["voxel"] = voxel

    gap, analysis, region_mask = _gap_estimate(v0, t0)
    damaged = _damaged_bbox(v0, t0, region_mask)
    if damaged is not None:
        analysis["damaged_bbox_fraction"] = (
            float(np.linalg.norm(damaged[1] - damaged[0])) / diag if diag else 0.0)

    if r is None:
        ladder = _r_ladder(gap, voxel, diag, max_tries)
        if use_sign_field:
            # Pass 0: the raw sign field (r = 0), no closing.  It is adopted
            # only when it is reload-watertight and passes the detail gate.
            ladder = [0.0] + ladder
    else:
        ladder = [float(r)]
    report["r_ladder"] = [round(x, 6) for x in ladder]
    report["analysis"] = dict(analysis)

    use_local = bool(local) if local is not None else (
        damaged is not None and gap > 0.0
        and analysis["damaged_bbox_fraction"] < LOCAL_MAX_BBOX_FRACTION)
    local_box = None
    if use_local and damaged is not None:
        margin = max(LOCAL_WINDOW_MARGIN * ladder[-1], 0.02 * diag)
        local_box = np.vstack([damaged[0] - margin, damaged[1] + margin])
    box_arg = None
    if box is not None:
        box_arg = np.asarray(box, dtype=np.float64)
        use_local = False
        local_box = None

    rng = np.random.default_rng(20260928)
    best = None
    for attempt, rval in enumerate(ladder):
        report["tries"] = attempt + 1
        is_sign = float(rval) <= 0.0
        # A sign field is a global isosurface; a local window is meaningless
        # for r = 0, so Pass 0 always runs whole-mesh.
        attempt_local = use_local and not is_sign
        # r = 0 keeps the raw field, so the projection / behind-on-healthy
        # distance scales with the voxel (the isosurface lies within ~1 voxel).
        r_eff = float(rval) if float(rval) > 0.0 else float(voxel)
        mode = ("sign_field" if is_sign
                else ("local" if attempt_local else "whole"))
        try:
            ev, et, info = sutura_geom.morph_close(
                v0, t0, float(rval), voxel,
                local_box if attempt_local else box_arg, bool(fill_cavities),
                **raystab_kwargs)
        except Exception:  # noqa: BLE001
            continue
        ev, et = _as_f64(ev), _as_i64(et)
        if len(et) == 0:
            continue
        if attempt_local:
            sv, st, ok = _stitch_local(ev, et, v0, t0)
            if ok:
                ev, et = sv, st
            else:
                mode = "whole"
                try:
                    ev, et, info = sutura_geom.morph_close(
                        v0, t0, float(rval), voxel, box_arg, bool(fill_cavities),
                        **raystab_kwargs)
                    ev, et = _as_f64(ev), _as_i64(et)
                except Exception:  # noqa: BLE001
                    continue
            if len(et) == 0:
                continue

        # --- refine the envelope to the median healthy edge BEFORE projecting --
        # (a coarse envelope cannot carry the original's detail through the
        # projection; the reviewer-required order is refine -> project)
        raw_ev, raw_et = ev, et
        raw_si = _si_count(raw_ev, raw_et) if si_check else None
        if remesh and len(et) <= REMESH_MAX_FACES:
            hv0, ht0 = _referenced_only(v0, t0[~region_mask]) if (
                region_mask is not None and (~region_mask).any()) else (v0, t0)
            med_h = _median_edge_length(hv0, ht0) if len(ht0) else 0.0
            tgt = target_edge if target_edge else med_h
            if tgt and tgt > 0:
                # refine (split) only: a coarse envelope gains vertices, a
                # finer one is left alone so the projection keeps its shape
                ev2, et2 = _split_long_edges(ev, et, tgt)
                refine_cap = min(REFINE_MAX_FACES,
                                 max(REFINE_FACE_FACTOR * len(t0), 2000))
                if len(et2) and len(et2) <= refine_cap:
                    ev, et = ev2, et2
                    report["remeshed"] = True
        ref_si = _si_count(ev, et) if si_check else None
        si_rolled = False
        if raw_si is not None and ref_si is not None and ref_si > raw_si:
            ev, et = raw_ev, raw_et
            ref_si = raw_si
            si_rolled = True
            report["remeshed"] = False

        # wrap / projection (detail-preserving)
        pv, healthy = _project_envelope(ev, et, v0, t0, r_eff)
        wrapped_v, wrapped_t = pv, et
        si_before = ref_si
        si_after = ref_si
        rolled = si_rolled

        # Stitch (P-WELD) + X-Ray (honest reload verdict)
        wv, wt, pw = _p_weld(wrapped_v, wrapped_t, ml)
        h, nm = reload_strict_holes_nm(wv, wt)

        # guards / detail loss
        h1 = None
        hh = None
        hh_behind = None
        if guard:
            # informational: all-input -> result (a geometry-inventing close
            # intentionally does not reproduce the damaged input)
            h1, _m1 = _one_sided_hausdorff(v0, t0, wv, wt, diag, rng=rng)
            hh_behind = _behind_on_healthy(
                v0, t0, region_mask, wv, wt, diag, r_eff, rng)
        # exact healthy original -> result deviation (per original vertex)
        detail = _detail_loss(v0, t0, wv, wt, region_mask, diag, detail_tol)
        if guard and diag > 0.0:
            hh = detail["max"] / diag
        fidelity_ok = (not guard) or (hh is None) or (hh <= HEALTHY_HAUSDORFF_MAX)

        # Hybrid fallback: keep the original healthy triangles verbatim and
        # close only the damaged region (needs the pymeshlab repair path).  The
        # envelope is used for the r decision; when it is too coarse to carry
        # the healthy detail through the projection, this guarantees zero
        # deviation on the healthy surface.
        if (ml is not None and region_mask is not None
                and not (h == 0 and nm == 0 and fidelity_ok)):
            hy = _hybrid_close(v0, t0, region_mask, ml)
            if hy is not None:
                hy_h, hy_nm = reload_strict_holes_nm(*hy)
                if hy_h == 0 and hy_nm == 0:
                    wv, wt = hy
                    h, nm = hy_h, hy_nm
                    mode = "hybrid"
                    if guard:
                        h1, _ = _one_sided_hausdorff(
                            v0, t0, wv, wt, diag, rng=rng)
                        hh_behind = _behind_on_healthy(
                            v0, t0, region_mask, wv, wt, diag, r_eff, rng)
                    detail = _detail_loss(
                        v0, t0, wv, wt, region_mask, diag, detail_tol)
                    if guard and diag > 0.0:
                        hh = detail["max"] / diag
                    fidelity_ok = ((not guard) or (hh is None)
                                   or (hh <= HEALTHY_HAUSDORFF_MAX))
                    pw = {"applied": False, "method": "hybrid-verbatim"}

        watertight = (h == 0 and nm == 0)
        cand = {
            "v": wv, "t": wt, "holes": int(h), "non_manifold": int(nm),
            "r": float(rval), "r_eff": float(r_eff), "mode": mode,
            "attempt": attempt,
            "watertight": watertight, "fidelity_ok": bool(fidelity_ok),
            "h1": h1, "hh": hh, "hh_behind": hh_behind,
            "projected_fraction": float(healthy.mean()),
            "dims": list(info.get("dims", [])),
            "voxel": float(info.get("voxel", voxel)),
            "remeshed": bool(report["remeshed"]),
            "si_before": si_before, "si_after": si_after, "si_rolled_back": rolled,
            "pweld": pw, "detail": detail,
            "raystab": info.get("raystab"),
        }
        # A sign-field (r = 0) candidate must NOT seed ``best``: if it is
        # watertight but fails the fidelity gate it could otherwise never be
        # replaced by a closing-ladder candidate (both have 0 holes+nm, so the
        # monotonic comparison below is False) and would be adopted despite a
        # shape-changing result.  Pass 0 is exploratory; only a
        # watertight AND fidelity-clean candidate is ever recorded.
        if not is_sign:
            if best is None or (cand["holes"] + cand["non_manifold"]) < (
                    best["holes"] + best["non_manifold"]):
                best = cand
        if watertight and fidelity_ok:
            best = cand
            break

    if best is None:
        report["error"] = "morph_close produced no candidate"
        report["seconds"] = round(time.perf_counter() - t_start, 3)
        return v0, t0, report

    detail = best["detail"]
    report.update({
        "ok": bool(best["watertight"]),
        "category": "watertight" if best["watertight"] else "partial",
        "r_used": round(best["r"], 6),
        "mode": best["mode"],
        "dims": best["dims"],
        "voxel": best["voxel"],
        "projected_fraction": round(best["projected_fraction"], 4),
        "remeshed": bool(best["remeshed"]),
        "si_before": best["si_before"],
        "si_after": best["si_after"],
        "si_rolled_back": bool(best["si_rolled_back"]),
        "hausdorff_one_sided": (round(best["h1"], 5) if best["h1"] is not None else None),
        "hausdorff_healthy": (round(best["hh"], 5) if best["hh"] is not None else None),
        "hausdorff_healthy_behind": (round(best["hh_behind"], 5)
                                     if best.get("hh_behind") is not None else None),
        "fidelity_ok": bool(best["fidelity_ok"]),
        "holes": best["holes"],
        "non_manifold": best["non_manifold"],
        "p_weld": best["pweld"],
        "raystab": best.get("raystab"),
        "faces_after": int(len(best["t"])),
        "detail_max_mm": round(detail["max"], 5),
        "detail_mean_mm": round(detail["mean"], 5),
        "detail_tolerance_mm": round(detail["tolerance"], 5),
        "detail_area_moved": round(detail["area_moved_fraction"], 5),
        "per_vertex_deviation": detail["per_vertex_deviation"],
    })
    report["detail_loss"] = report["hausdorff_healthy"]
    report["warnings"] = _warnings_for(best.get("r_eff", best["r"]), detail, diag)
    if best["watertight"] and not best["fidelity_ok"]:
        report["warnings"].append({
            "code": "fidelity",
            "message_en": (
                "Graft closed the mesh but the healthy-region deviation "
                f"({report['hausdorff_healthy']}) exceeds the tolerance; the "
                "result is watertight but its healthy shape may have changed."),
            "message_tr": (
                "Graft ağı kapattı ancak sağlıklı bölge sapması "
                f"({report['hausdorff_healthy']}) toleransı aşıyor; sonuç su "
                "geçirmez fakat sağlıklı şekli değişmiş olabilir."),
        })
    report["seconds"] = round(time.perf_counter() - t_start, 3)
    return _as_f64(best["v"]), _as_i64(best["t"]), report


if __name__ == "__main__":
    import sys
    import json
    path = sys.argv[1]
    _m = trimesh.load(path, force="mesh")
    ov, ot, rep = shell_wrap(np.asarray(_m.vertices, np.float64),
                             np.asarray(_m.faces, np.int64))
    rep = {k: v for k, v in rep.items() if k != "per_vertex_deviation"}
    print(json.dumps(rep, indent=2, default=str))
