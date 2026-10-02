#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Dressing (#16, variable-viscosity volumetric skinning).

A surgical sibling of Graft (#13): the input is dipped in a spatially varying
"liquid", and the level set ``F(x) = s(x) - r(x)`` is extracted, where ``s`` is
the generalized-winding signed distance and ``r`` a viscosity radius that is
thin over detailed healthy surface and thicker over damage.  The isosurface of a
signed field is 2-manifold and self-intersection-free by construction, which is
what makes Dressing the tool for whole-shell folds where local patch excision
fails.  It deliberately does NOT project back onto the input surface (measured:
that re-introduces thousands of self-intersections); fidelity comes from a thin
base radius instead.

The backend is the numpy prototype (``sdf_grid`` -> variable radius -> marching
tetrahedra).  When the Rust core grows ``sutura_geom.dressing_coat`` this module
feature-detects it and delegates the field build + extraction to it, falling
back to numpy on any error.

Pure API::

    dressing_coat(verts, tris, *, voxel=None, intensity=None, grid_budget=None,
                  r_base=None, r_max=None, sigma=None, ml=None, guard=True,
                  time_budget=None, drain=None, defects=None,
                  r_max_scale=None, sigma_scale=None)
                  -> (verts_out, tris_out, report)

No file is written.  Decimation uses PyMeshLab when a live ``ml`` module is
passed; without it the raw (well-formed) coat is returned unchanged.
"""
from __future__ import annotations

import time

import numpy as np

import sutura_geom
from defects import detect as detect_defects
from sutura_engine.graft import (
    _as_f64,
    _as_i64,
    _diag,
    _median_edge_length,
    _one_sided_hausdorff,
    _p_weld,
)
from repair import reload_strict_holes_nm

METHOD_NUMBER = 16
DISPLAY_NAME = "Dressing"
DISPLAY_NAME_FULL = "Dressing (viscosity coat)"

# --- per-intensity parameters (integration plan §3.4) ----------------------
# ``voxel_div`` is the preset's [min, max] diagonal divisor; the actual voxel is
# feature-anchored and only clipped into this band.  ``r_preset`` caps the
# bridging radius, ``max_hausdorff`` is the coat->input fidelity gate (relative
# to the bbox diagonal) and ``time_budget`` the (soft) per-mesh budget.
DRESSING_BY_INTENSITY = {
    'quick':    {'voxel_div': (120, 200), 'r_preset': 4.0,
                 'max_hausdorff': 0.0150, 'time_budget': 30.0,
                 'drain_factor': 0.0},
    'balanced': {'voxel_div': (200, 350), 'r_preset': 2.5,
                 'max_hausdorff': 0.0080, 'time_budget': 60.0,
                 'drain_factor': 1.0},
    'thorough': {'voxel_div': (300, 500), 'r_preset': 1.8,
                 'max_hausdorff': 0.0040, 'time_budget': 180.0,
                 'drain_factor': 1.0},
    'extreme':  {'voxel_div': (450, 800), 'r_preset': 1.2,
                 'max_hausdorff': 0.0025, 'time_budget': 300.0,
                 'drain_factor': 1.25},
}
DRESSING_DEFAULT_INTENSITY = 'balanced'

# Drain ("erode-back") amounts as multiples of ``r_base`` (the offset report's
# recommendation): the field is drained by ``F = s - r(x) + drain_factor*r_base``
# so the healthy isosurface re-centres on the original boundary.  Full drain
# (1.0) cuts healthy growth ~40 % with a ~4 um median inward dip (harmless);
# deep (1.25) trades ~70 um inward for tighter mating faces; off (0.0) keeps
# the plain outward coat.
DRAIN_MODES = {
    'none': 0.0,
    'off': 0.0,
    'half': 0.5,
    'full': 1.0,
    'deep': 1.25,
}

# Which input defects feed the viscosity field's ``d_defect`` mask.  ``'all'``
# (default) is holes + non-manifold + self-intersections, the historical
# behaviour; ``'holes_nm'`` drops the self-intersection term, which on dense
# scans otherwise saturates the mask and bridges over healthy surface.  Scale
# factors multiply the resolved ``r_max`` / ``sigma`` (1.0 = the current
# default) so a narrower, tighter coat can be requested without new presets.
DEFECT_SETS = ('all', 'holes_nm')
DEFECT_SET_DEFAULT = 'all'

# output face budget: decimate the coat toward the input face count
FACE_BUDGET_MIN = 50_000
FACE_BUDGET_MAX = 500_000

# grid cell budget per preset (the numpy prototype needs the head-room that
# framebaroque's 6.9 M-voxel slab required); ``sdf_grid`` still applies its own
# absolute MAX_VOXELS cap on top.
DRESSING_GRID_BUDGET = {
    'quick': 2_000_000,
    'balanced': 8_000_000,
    'thorough': 20_000_000,
    'extreme': 40_000_000,
}

SI_MAX_FACES = int(getattr(sutura_geom, "SI_MAX_FACES", 150_000))
_ROUNDS = 1_000_000

# --- post-coat component cleanup ------------------------------------------
# The extracted level set is a triangle soup; marching-tets also leaves tiny
# disconnected shells (band boundaries / grazing surfaces) and stray inverted
# fragments inside the solid, which inflate the output's connected-component
# count.  A component is extraction DEBRIS when it is below a face-count floor
# OR below an absolute voxel-volume floor OR below a relative-volume floor set
# by the largest component (measured on the 145065/63785/1038439 coats: debris
# is <= 932 faces and <= ~0.6 voxel^3, the smallest genuine shell is 10 176
# faces and ~5 000 voxel^3).  A surviving inverted component is kept only when
# it is nested inside a kept positive component AND the input's generalized
# winding number at its centre is ~0, i.e. the input is genuinely empty there
# (a true cavity).  Everything else is dropped.  ``CLEANUP_*`` are module
# constants so the thresholds are inspectable and reversible.
CLEANUP_MIN_FACES = 64
CLEANUP_ABS_VOXELS = 32.0
CLEANUP_REL_VOLUME = 1e-3

# --- marching tetrahedra tables (from the validated prototype) -------------
_CUBE = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                  [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], dtype=np.int64)
# 6 tetrahedra around the cube body diagonal 0-6 (consistent tiling).
_TETS = np.array([[0, 5, 1, 6], [0, 1, 2, 6], [0, 2, 3, 6],
                  [0, 3, 7, 6], [0, 7, 4, 6], [0, 4, 5, 6]], dtype=np.int64)
_TET_EDGES = np.array([[0, 1], [1, 2], [2, 0], [0, 3], [1, 3], [2, 3]],
                      dtype=np.int64)


def _order_quad(cross, s):
    neg = [i for i in range(4) if s[i] == 1]
    pos = [i for i in range(4) if s[i] == 0]
    cycle = [(neg[0], pos[0]), (pos[0], neg[1]),
             (neg[1], pos[1]), (pos[1], neg[0])]
    out = []
    for a, b in cycle:
        e = (a, b)
        out.append(e if e in cross else (b, a))
    return out


def _case_table():
    table = {}
    for case in range(16):
        s = [(case >> i) & 1 for i in range(4)]
        cross = [tuple(e) for e in _TET_EDGES if s[e[0]] != s[e[1]]]
        tris = []
        if len(cross) == 3:
            tris = [tuple(cross)]
        elif len(cross) == 4:
            o = _order_quad(cross, s)
            tris = [(o[0], o[1], o[2]), (o[0], o[2], o[3])]
        table[case] = tris
    return table


_CASES = _case_table()


def _weld_positions(v, t, tol):
    """Merge vertices that coincide in space (distinct edge-keys can give the
    same point when F hits exactly 0 at a grid node).  Pure numpy."""
    if len(v) == 0:
        return v, t
    q = np.round(v / tol).astype(np.int64)
    uq, inv = np.unique(q, axis=0, return_inverse=True)
    inv = inv.reshape(-1)
    nv = np.zeros((len(uq), 3), dtype=np.float64)
    np.add.at(nv, inv, v)
    nv /= np.bincount(inv)[:, None]
    nt = inv[t]
    ok = (nt[:, 0] != nt[:, 1]) & (nt[:, 1] != nt[:, 2]) & (nt[:, 0] != nt[:, 2])
    return nv, nt[ok]


def _reorient_faces(v, t):
    """Make the face orientation coherent (BFS across shared edges) and flip
    the whole mesh so its signed volume is positive (outward normals).

    The marching-tetrahedra case table is topologically closed but can leave
    individual faces inconsistently wound; downstream stage 2 / winding relies
    on coherent orientation.  Pure numpy, O(F) for a manifold mesh."""
    t = np.asarray(t, dtype=np.int64).copy()
    if len(t) == 0:
        return t
    edge_to_faces = {}
    for f in range(len(t)):
        a, b, c = int(t[f, 0]), int(t[f, 1]), int(t[f, 2])
        for u, w in ((a, b), (b, c), (c, a)):
            key = (u, w) if u < w else (w, u)
            edge_to_faces.setdefault(key, []).append((f, 1 if u < w else -1))
    flip = np.zeros(len(t), dtype=bool)
    seen = np.zeros(len(t), dtype=bool)
    for start in range(len(t)):
        if seen[start]:
            continue
        seen[start] = True
        stack = [start]
        while stack:
            f = stack.pop()
            a, b, c = int(t[f, 0]), int(t[f, 1]), int(t[f, 2])
            for u, w in ((a, b), (b, c), (c, a)):
                key = (u, w) if u < w else (w, u)
                dirf = 1 if u < w else -1
                if flip[f]:
                    dirf = -dirf
                for g, dirg in edge_to_faces.get(key, ()):
                    if g == f or seen[g]:
                        continue
                    flip[g] = bool(dirg != -dirf)
                    seen[g] = True
                    stack.append(g)
    t[flip] = t[flip][:, ::-1]
    tri = v[t]
    vol = float(np.einsum('ij,ij->i', tri[:, 0],
                          np.cross(tri[:, 1], tri[:, 2])).sum())
    if vol < 0:
        t = t[:, ::-1].copy()
    return t


def _assemble(keys_out, pos_out):
    if not keys_out:
        return np.zeros((0, 3)), np.zeros((0, 3), dtype=np.int64)
    keys = np.concatenate(keys_out, axis=0).reshape(-1).astype(np.int64)
    pos = np.concatenate(pos_out, axis=0).reshape(-1, 3).astype(np.float64)
    uniq, first, inverse = np.unique(keys, return_index=True, return_inverse=True)
    verts = pos[first]
    tris = inverse.reshape(-1, 3).astype(np.int64)
    a = verts[tris[:, 0]]
    b = verts[tris[:, 1]]
    c = verts[tris[:, 2]]
    area = np.linalg.norm(np.cross(b - a, c - a), axis=1)
    tris = tris[area > 1e-12]
    used = np.unique(tris)
    remap = np.full(len(verts), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return verts[used], remap[tris].astype(np.int32)


def _marching_tets(F, origin, voxel, slab=8):
    """Extract the F=0 isosurface.  Returns ``(verts f64 (V,3), tris i32 (T,3))``.

    Port of the validated prototype extractor: outputs of a scalar field are
    2-manifold and self-intersection-free by construction, and crossing points
    are welded exactly by keying each to its integer grid edge.  The two
    prototype fixes are kept (per-triangle edge grouping; positional weld).
    """
    F = np.ascontiguousarray(F, dtype=np.float32)
    nx, ny, nz = F.shape
    ox, oy, oz = float(origin[0]), float(origin[1]), float(origin[2])
    keys_out = []
    pos_out = []
    for k0 in range(0, nz - 1, slab):
        k1 = min(k0 + slab, nz - 1)
        ii, jj, kk = np.meshgrid(np.arange(nx - 1), np.arange(ny - 1),
                                 np.arange(k0, k1), indexing='ij')
        I = ii.ravel(); J = jj.ravel(); K = kk.ravel()
        if I.size == 0:
            continue
        px = [None] * 8; py = [None] * 8; pz = [None] * 8; cv = [None] * 8
        for c in range(8):
            dx, dy, dz = _CUBE[c]
            ci = I + dx; cj = J + dy; ck = K + dz
            px[c] = (ox + ci * voxel).astype(np.float32)
            py[c] = (oy + cj * voxel).astype(np.float32)
            pz[c] = (oz + ck * voxel).astype(np.float32)
            cv[c] = F[ci, cj, ck]
        for t in range(6):
            lt = _TETS[t]
            vals = np.stack([cv[lt[0]], cv[lt[1]], cv[lt[2]], cv[lt[3]]], axis=1)
            neg = vals < 0.0
            case = (neg[:, 0].astype(np.int64) | (neg[:, 1].astype(np.int64) << 1)
                    | (neg[:, 2].astype(np.int64) << 2)
                    | (neg[:, 3].astype(np.int64) << 3))
            npos = neg.sum(axis=1)
            active = (npos > 0) & (npos < 4)
            if not active.any():
                continue
            for caseval, tris in _CASES.items():
                if not tris:
                    continue
                m = active & (case == caseval)
                if not m.any():
                    continue
                idx = np.flatnonzero(m)
                for tri in tris:
                    ks = []; ps = []
                    for e in tri:
                        u, v = int(e[0]), int(e[1])
                        au = vals[idx, u]; av = vals[idx, v]
                        tpar = (au / (au - av)).astype(np.float32)
                        ex = px[lt[u]][idx] + tpar * (px[lt[v]][idx] - px[lt[u]][idx])
                        ey = py[lt[u]][idx] + tpar * (py[lt[v]][idx] - py[lt[u]][idx])
                        ez = pz[lt[u]][idx] + tpar * (pz[lt[v]][idx] - pz[lt[u]][idx])
                        gu = _CUBE[lt[u]]; gv = _CUBE[lt[v]]
                        ia = (((I[idx] + gu[0]).astype(np.int64) * ny
                               + (J[idx] + gu[1])) * nz + (K[idx] + gu[2]))
                        ib = (((I[idx] + gv[0]).astype(np.int64) * ny
                               + (J[idx] + gv[1])) * nz + (K[idx] + gv[2]))
                        lo = np.minimum(ia, ib); hi = np.maximum(ia, ib)
                        ks.append(lo * (nx * ny * nz) + hi)
                        ps.append(np.stack([ex, ey, ez], axis=1))
                    # group the 3 edge vertices of THIS triangle per cube
                    keys_out.append(np.stack(ks, axis=1))
                    pos_out.append(np.stack(ps, axis=1))
    v, t = _assemble(keys_out, pos_out)
    return _weld_positions(v, t, 1e-5 * voxel)


# ---------------------------------------------------------------------------
# detail / viscosity metrics
# ---------------------------------------------------------------------------
def resolve_dressing_voxel(verts, tris, diag, intensity=None, grid_budget=None):
    """Feature-anchored voxel size, clipped by the preset diagonal band.

    The voxel follows the characteristic feature size (``median_edge / 1.5``),
    not only the bounding box, so a coarse open fragment (thingi10k_100827, 71
    faces, diag 50.8 mm) is no longer oversampled at ``diag / 200`` (which
    produced 1.06 M coat faces and 7.3 % deviation).  Meshes with very few
    faces skip the fine end of the preset band; an explicit ``grid_budget``
    coarsens the voxel until the grid fits.
    """
    if intensity not in DRESSING_BY_INTENSITY:
        intensity = DRESSING_DEFAULT_INTENSITY
    spec = DRESSING_BY_INTENSITY[intensity]
    div_min, div_max = spec['voxel_div']
    diag = float(diag)
    if not (diag > 0.0):
        return 1.0
    med = _median_edge_length(verts, tris)
    if med > 0.0:
        v_feature = med / 1.5
    else:
        v_feature = diag / 200.0
    v_preset_min = diag / float(div_max)
    v_preset_max = diag / float(div_min)
    if len(tris) < 500:
        v_target = max(v_feature, v_preset_min)
    else:
        v_target = float(np.clip(v_feature, v_preset_min, v_preset_max))
    if grid_budget is not None and len(verts):
        ext = np.maximum(verts.max(axis=0) - verts.min(axis=0), 1e-12)
        for _ in range(120):
            dims = np.ceil(ext / v_target).astype(np.int64) + 6
            if float(np.prod(dims)) <= grid_budget:
                break
            v_target *= 1.15
    return float(v_target)


def resolve_viscosity(ell_median, voxel, intensity=None, gap=None,
                      r_max_scale=None, sigma_scale=None):
    """``(r_base, r_max, sigma)`` for one mesh.

    Thin base radius over healthy detailed surface (the fidelity mechanism;
    framebaroque r_base ~= 0.17 mm -> 0.06 % of the diagonal), a preset-capped
    bridging radius near defects (also capped at 45 % of the largest gap, so a
    coat over a big opening cannot balloon past the model), and a transition
    width tied to the feature size / voxel.  ``r_max_scale`` / ``sigma_scale``
    multiply the resolved ``r_max`` / ``sigma`` (``None``/``1.0`` = default).
    """
    if intensity not in DRESSING_BY_INTENSITY:
        intensity = DRESSING_DEFAULT_INTENSITY
    r_preset = DRESSING_BY_INTENSITY[intensity]['r_preset']
    r_base = float(np.clip(0.3 * ell_median, 0.25 * voxel, 0.8 * voxel))
    r_max = float(min(r_preset, 3.5 * ell_median)) if ell_median > 0 else r_preset
    if gap is not None and gap > 0:
        r_max = min(r_max, 0.45 * float(gap))
    if r_max_scale is not None:
        r_max = r_max * float(r_max_scale)
    r_max = max(r_max, r_base)
    sigma = float(max(2.0 * ell_median, 2.0 * voxel))
    if sigma_scale is not None:
        sigma = sigma * float(sigma_scale)
    return r_base, r_max, sigma


def _normalize_scale(scale):
    """A positive float scale factor, or ``None`` for the default (1.0)."""
    if scale is None:
        return None
    try:
        s = float(scale)
    except (TypeError, ValueError):
        return None
    return s if s > 0.0 else None


def _defect_vertex_indices(v, t, defects=None):
    """``(vertex indices, gap)`` touched by holes, non-manifold regions and
    (unless ``defects='holes_nm'``, and when within the Rust cap)
    self-intersecting faces.  ``gap`` is the largest hole diameter, used to cap
    the bridging radius."""
    mode = defects if defects in DEFECT_SETS else DEFECT_SET_DEFAULT
    idx = set()
    gap = 0.0
    det = detect_defects(v, t, with_indices=True)
    for h in det['holes']:
        idx.update(int(i) for i in h.get('verts_idx', ()))
        gap = max(gap, float(h.get('diameter') or 0.0))
    for nm in det['non_manifold']:
        idx.update(int(i) for i in nm.get('verts_idx', ()))
    if mode == 'all' and len(t) <= SI_MAX_FACES:
        try:
            mask, _count = sutura_geom.self_intersecting_faces(v, t)
            mask = np.asarray(mask)
            if mask.dtype != bool:
                mask = mask.astype(bool)
            if mask.any():
                idx.update(int(i) for i in np.unique(t[mask]))
        except Exception:  # noqa: BLE001 - SI detection is best-effort
            pass
    return np.array(sorted(idx), dtype=np.int64), gap


def _variable_radius(dims, origin, voxel, defect_idx, verts, r_base, r_max, sigma):
    """Radius grid and defect influence ``g`` from the EDT of the defect mask.

    Returns ``(r_grid, g)`` where ``r = r_base + (r_max - r_base) * g`` and
    ``g = exp(-d_defect^2 / 2 sigma^2)`` is the defect influence (``~0`` on
    healthy surface, ``~1`` at a defect).  ``g`` is reused by the drain so the
    numpy path matches the Rust core's ``F = s - r + drain * (1 - g)``.
    """
    from scipy import ndimage
    dx, dy, dz = int(dims[0]), int(dims[1]), int(dims[2])
    if len(defect_idx) == 0:
        return (np.full((dx, dy, dz), np.float32(r_base)),
                np.zeros((dx, dy, dz), np.float32))
    dvx = (verts[defect_idx] - origin) / voxel
    gi = np.clip(np.round(dvx[:, 0]).astype(np.int64), 0, dx - 1)
    gj = np.clip(np.round(dvx[:, 1]).astype(np.int64), 0, dy - 1)
    gk = np.clip(np.round(dvx[:, 2]).astype(np.int64), 0, dz - 1)
    mask = np.zeros((dx, dy, dz), dtype=bool)
    mask[gi, gj, gk] = True
    d_defect = ndimage.distance_transform_edt(~mask).astype(np.float32) * np.float32(voxel)
    g = np.exp(-0.5 * (d_defect / sigma) ** 2).astype(np.float32)
    r_grid = (r_base + (r_max - r_base) * g).astype(np.float32)
    return r_grid, g


# ---------------------------------------------------------------------------
# extraction (Rust when available, numpy prototype otherwise)
# ---------------------------------------------------------------------------
def _rust_signature_has_drain(fn):
    """True when the installed ``sutura_geom.dressing_coat`` accepts a ``drain``
    keyword.  The c2f2116 build does not; a later commit adds it.  Inspected
    once per call (cheap; the binding is a builtin without a Python signature,
    so probe with an empty keyword call and treat "unexpected keyword drain" as
    absent)."""
    try:
        import inspect
        params = inspect.signature(fn).parameters
        if params:
            return 'drain' in params
    except (TypeError, ValueError):
        pass
    # Builtin without an introspectable signature: feature-probe via a tiny
    # call that raises TypeError mentioning 'drain' only if the kwarg is
    # unknown.  Any other outcome (success or a different error) is treated as
    # "unknown", so the caller falls back to the drain-free numpy path.
    probe = np.zeros((0, 3), dtype=np.float64)
    try:
        fn(probe, np.zeros((0, 3), dtype=np.int32), drain=0.0)
        return True
    except TypeError as e:
        return 'drain' not in str(e)
    except Exception:  # noqa: BLE001
        return False


def _rust_dressing_coat(v, t, voxel, r_base, r_max, sigma, defect_idx, box,
                        drain_delta=0.0):
    """Delegate to ``sutura_geom.dressing_coat`` when the installed extension
    provides it.  Returns ``(verts, tris, info)`` or ``None``.

    The current binding signature is ``dressing_coat(verts, tris,
    defect_pts=None, voxel=None, r_base=None, r_max=None, sigma=None,
    band_voxels=2.0, margin_voxels=3.0, drain=0.0)`` where ``defect_pts`` is an
    Nx3 point array (not vertex indices).  The older c2f2116 build has no
    ``drain`` parameter, so when a positive drain is requested and the build
    cannot take it, this returns ``None`` and the caller uses the numpy path
    (which applies the drain in the level set).  Any signature mismatch or
    runtime error falls back to numpy (never raises).
    """
    fn = getattr(sutura_geom, 'dressing_coat', None)
    if fn is None:
        return None
    fn_takes_drain = _rust_signature_has_drain(fn)
    if drain_delta and not fn_takes_drain:
        # No drain support in this build -> numpy path applies it.
        return None
    defect_pts = (np.asarray(v, np.float64)[defect_idx]
                  if len(defect_idx) else np.zeros((0, 3), np.float64))
    kwargs = dict(defect_pts=defect_pts, voxel=float(voxel),
                  r_base=float(r_base), r_max=float(r_max), sigma=float(sigma))
    if fn_takes_drain:
        kwargs['drain'] = float(drain_delta)
    try:
        out = fn(v, t, **kwargs)
    except Exception:  # noqa: BLE001
        return None
    try:
        verts, tris = out[0], out[1]
        info = out[2] if len(out) > 2 else {}
        return (np.asarray(verts, np.float64), np.asarray(tris, np.int64),
                dict(info or {}))
    except Exception:  # noqa: BLE001
        return None


def _numpy_coat(v, t, voxel, r_base, r_max, sigma, defect_idx, box,
                drain_delta=0.0):
    """Numpy prototype path: GWN signed field -> variable radius -> MT.

    ``drain_delta`` is the healthy-region erode-back in mm, applied to the
    level set exactly like the Rust core: ``F = s - r + drain * (1 - g)`` where
    ``g`` is the defect influence.  Healthy cells (``g ~ 0``) get the full
    drain (their isosurface re-centres on the input boundary); defect cells
    (``g ~ 1``) keep their coverage (offset report §1)."""
    w, u, s, info = sutura_geom.sdf_grid(v, t, voxel=voxel, box=box)
    voxel = float(info['voxel'])
    origin = np.asarray(info['origin'], dtype=np.float64)
    dims = tuple(int(x) for x in info['dims'])
    r_grid, g = _variable_radius(dims, origin, voxel, defect_idx, v,
                                 r_base, r_max, sigma)
    F = np.asarray(s, dtype=np.float32) - r_grid
    if drain_delta:
        F += np.float32(drain_delta) * (np.float32(1.0) - g)
    del s, r_grid, g
    cv, ct = _marching_tets(F, origin, voxel, slab=8)
    del F
    meta = {
        'dims': list(dims),
        'origin': origin.tolist(),
        'voxel': voxel,
        'voxels': int(np.prod(dims)),
        'coarsened': bool(info.get('caps_coarsened', False)),
    }
    return cv, ct, meta


# ---------------------------------------------------------------------------
# decimation with rollback guard
# ---------------------------------------------------------------------------
def _target_faces(n_input_faces):
    return int(max(min(int(n_input_faces), FACE_BUDGET_MAX), FACE_BUDGET_MIN))


def _qem_decimate(ml, v, t, target_faces):
    """PyMeshLab quadric edge collapse that preserves topology / normals.

    Configuration from the decimation benchmark (`/tmp/v073/dressing/decim/
    report.md`, framebaroque 1.36 M-face coat): ``optimalplacement=False``
    ("subset placement") with ``planarquadric=True`` yields **0 self-
    intersections**, sub-micron fidelity to the coat and ~20 s at a 400k
    target, whereas unconstrained placement introduces up to 30 SI faces.  A
    2-step schedule is used when the target is at or below half the raw coat,
    which the report found also reaches 0 SI even with optimal placement.
    Returns ``(v, t)``."""
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(v, np.float64),
                        face_matrix=np.asarray(t, np.int32)))

    def _collapse(target):
        ms.apply_filter('meshing_decimation_quadric_edge_collapse',
                        targetfacenum=int(target),
                        preservetopology=True,
                        preservenormal=True,
                        planarquadric=True,
                        planarweight=0.001,
                        optimalplacement=False,
                        autoclean=True)

    if len(t) > 2 * int(target_faces) and int(target_faces) <= 250_000:
        _collapse(min(len(t) // 2, 400_000))
    _collapse(target_faces)
    m = ms.current_mesh()
    return (np.asarray(m.vertex_matrix(), np.float64),
            np.asarray(m.face_matrix(), np.int64))


def _exact_si(v, t):
    """Rust exact proper self-intersection count, or ``None`` above the cap."""
    if len(t) == 0 or len(t) > SI_MAX_FACES:
        return None
    try:
        _mask, count = sutura_geom.self_intersecting_faces(v, t)
        return int(count)
    except Exception:  # noqa: BLE001
        return None


def si_face_count(v, t):
    """Exact proper self-intersection face count (``None`` when unmeasurable).

    Thin wrapper over :func:`_exact_si` so the repair ladder can use the same
    exact classifier as the adoption gate.
    """
    return _exact_si(_as_f64(v), _as_i64(t))


def signed_volume(verts, tris):
    """Signed volume of a triangle soup (float; orientation-sensitive)."""
    t = _as_i64(tris)
    if len(t) == 0:
        return 0.0
    v = _as_f64(verts)
    a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
    return float(np.einsum('ij,ij->i', a, np.cross(b, c)).sum() / 6.0)


def count_components(tris):
    """Vertex-connected face components (``None`` when scipy is unavailable)."""
    labels = _component_labels(_as_i64(tris))
    if labels is None:
        return None
    return int(len(np.unique(labels)))


def _vertex_normals(v, t):
    """Area-weighted per-vertex normals (falling back to +Z for isolated)."""
    a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
    fn = np.cross(b - a, c - a)
    vn = np.zeros_like(v)
    for k in range(3):
        np.add.at(vn, t[:, k], fn)
    ln = np.linalg.norm(vn, axis=1)
    bad = ln == 0
    ln[bad] = 1.0
    vn = vn / ln[:, None]
    vn[bad] = (0.0, 0.0, 1.0)
    return vn


def normal_angle_deviation(wv, wt, v0, t0, defect_idx=None):
    """Angle between each coat vertex normal and the nearest input vertex normal.

    Input vertex normals (area-weighted, like the coat's) are the smooth
    surface normal; comparing to a single input FACE normal would report ~55°
    at every sharp corner even for an identical mesh.  Returns ``{'p50',
    'p95', 'p99', 'max', 'n'}`` in degrees over the HEALTHY coat vertices
    (whose nearest input vertex is not a defect vertex), or ``None`` when scipy
    is unavailable.  This is the surface-direction counterpart of the Hausdorff
    fidelity gate: a voxel staircase / fluting loss keeps the position within
    tolerance but tilts the normals, which this metric exposes.
    """
    v0 = _as_f64(v0)
    t0 = _as_i64(t0)
    wv = _as_f64(wv)
    wt = _as_i64(wt)
    if len(t0) == 0 or len(wt) == 0 or len(wv) == 0:
        return None
    try:
        from scipy.spatial import cKDTree
        ivn = _vertex_normals(v0, t0)
        cvn = _vertex_normals(wv, wt)
        _dist, idx = cKDTree(v0).query(wv, k=1)
        idx = np.asarray(idx, np.int64)
        cosang = np.abs(np.einsum('ij,ij->i', cvn, ivn[idx]))
        ang = np.degrees(np.arccos(np.clip(cosang, 0.0, 1.0)))
        if defect_idx is not None and len(defect_idx):
            dset = np.zeros(len(v0), bool)
            dset[np.asarray(defect_idx, np.int64)] = True
            healthy = ~dset[idx]
        else:
            healthy = np.ones(len(wv), bool)
        vals = ang[healthy]
        if len(vals) == 0:
            return None
        return {
            'p50': float(np.percentile(vals, 50)),
            'p95': float(np.percentile(vals, 95)),
            'p99': float(np.percentile(vals, 99)),
            'max': float(vals.max()),
            'n': int(len(vals)),
        }
    except Exception:  # noqa: BLE001 - the metric is informational
        return None


def _drain_mode_name(factor):
    """The :data:`DRAIN_MODES` name for a drain ``factor`` (or ``'custom'``)."""
    for name in ('none', 'half', 'full', 'deep'):
        if abs(DRAIN_MODES[name] - float(factor)) < 1e-9:
            return name
    return 'custom'


def _resolve_drain(drain, intensity):
    """Resolve the drain to ``(delta_r, factor, mode)``.

    ``drain`` may be ``None`` (the preset's ``drain_factor`` default), a mode
    name in :data:`DRAIN_MODES`, or an explicit ``delta_r`` in mm.  ``factor``
    is the multiple of ``r_base`` (``None`` for an explicit delta); ``mode`` is
    the resolved name.
    """
    if drain is None:
        factor = float(DRESSING_BY_INTENSITY.get(
            intensity, DRESSING_BY_INTENSITY[DRESSING_DEFAULT_INTENSITY]
        )['drain_factor'])
        return factor, factor, _drain_mode_name(factor)
    if isinstance(drain, str):
        key = drain.strip().lower()
        if key not in DRAIN_MODES:
            key = 'none'
        factor = DRAIN_MODES[key]
        return factor, factor, key
    return float(drain), None, 'custom'


def _decimate_coat(ml, v, t, target_faces):
    """Decimate the coat toward ``target_faces`` with an atomic rollback.

    Adopts the decimated mesh only when it stays 0 holes / 0 non-manifold and
    introduces no exact self-intersection; otherwise the un-decimated coat is
    returned byte-for-byte.  ``ml is None`` skips decimation.
    """
    if len(t) <= target_faces * 1.05:
        return v, t, {'decimated': False, 'reason': 'already_within_budget',
                      'target': int(target_faces)}
    if ml is None:
        return v, t, {'decimated': False, 'reason': 'no_pymeshlab',
                      'target': int(target_faces)}
    try:
        dv, dt = _qem_decimate(ml, v, t, target_faces)
    except Exception as e:  # noqa: BLE001 - decimation is best-effort
        return v, t, {'decimated': False, 'reason': 'error: %s' % e,
                      'target': int(target_faces)}
    if len(dt) == 0:
        return v, t, {'decimated': False, 'reason': 'empty_result',
                      'target': int(target_faces)}
    h, nm = reload_strict_holes_nm(dv, dt)
    if h > 0 or nm > 0:
        return v, t, {'decimated': False, 'reason': 'topology_violation',
                      'holes': int(h), 'non_manifold': int(nm),
                      'target': int(target_faces)}
    si = _exact_si(dv, dt)
    if si is not None and si > 0:
        return v, t, {'decimated': False, 'reason': 'si_introduced',
                      'si': int(si), 'target': int(target_faces)}
    return dv, dt, {'decimated': True, 'reason': None,
                    'target': int(target_faces), 'faces': int(len(dt)),
                    'si': si}


# ---------------------------------------------------------------------------
# post-coat component cleanup
# ---------------------------------------------------------------------------
def _component_labels(t):
    """Vertex-connected face components as an ``(F,)`` int64 label array.

    Faces are grouped when they share a vertex.  Uses scipy's sparse
    connected-components; returns ``None`` when scipy is unavailable (the
    caller then leaves the coat untouched rather than risk a wrong split).
    """
    F = len(t)
    if F == 0:
        return None
    try:
        from scipy.sparse import coo_matrix
        from scipy.sparse.csgraph import connected_components
    except Exception:  # noqa: BLE001 - optional fast path
        return None
    e = np.concatenate([t[:, [0, 1]], t[:, [1, 2]], t[:, [2, 0]]])
    nv = int(t.max()) + 1
    g = coo_matrix((np.ones(len(e), np.int8), (e[:, 0], e[:, 1])),
                   shape=(nv, nv))
    _n, vlab = connected_components(g, directed=False)
    return vlab[t[:, 0]]


def _clean_coat_components(v0, t0, cv, ct, voxel):
    """Drop coat debris and stray inverted shells; keep material + cavities.

    Returns ``(cv, ct, info)``.  Never raises: any failure returns the input
    coat byte-for-byte with ``info['ran'] = False``.  See :data:`CLEANUP_MIN_FACES`
    for the decision rules.  ``info`` records the component counts, the removed
    counts/volume and the thresholds used.
    """
    info = {
        'ran': True,
        'components_before': 0,
        'components_after': 0,
        'kept_positive': 0,
        'kept_cavities': 0,
        'removed_debris': 0,
        'removed_negative': 0,
        'removed_other': 0,
        'removed_faces': 0,
        'removed_volume': 0.0,
        'min_faces': int(CLEANUP_MIN_FACES),
        'abs_voxels': float(CLEANUP_ABS_VOXELS),
        'rel_volume': float(CLEANUP_REL_VOLUME),
        'input_winding': False,
        'reason': None,
    }
    if len(ct) == 0:
        info['reason'] = 'empty'
        return cv, ct, info
    labels = _component_labels(ct)
    if labels is None:
        info['ran'] = False
        info['reason'] = 'scipy unavailable'
        return cv, ct, info
    uniq, inv = np.unique(labels, return_inverse=True)
    ncomp = int(len(uniq))
    info['components_before'] = ncomp

    V = cv.astype(np.float64)
    a, b, d = V[ct[:, 0]], V[ct[:, 1]], V[ct[:, 2]]
    fvol = np.einsum('ij,ij->i', a, np.cross(b, d)) / 6.0
    cvol = np.zeros(ncomp, np.float64)
    cfaces = np.zeros(ncomp, np.int64)
    cmin = np.full((ncomp, 3), np.inf)
    cmax = np.full((ncomp, 3), -np.inf)
    ccent = np.zeros((ncomp, 3), np.float64)
    np.add.at(cvol, inv, fvol)
    np.add.at(cfaces, inv, 1)
    np.add.at(ccent, inv, (a + b + d) / 3.0)
    np.minimum.at(cmin, inv, np.minimum(np.minimum(a, b), d))
    np.maximum.at(cmax, inv, np.maximum(np.maximum(a, b), d))
    ccent /= cfaces[:, None]

    largest = float(np.max(np.abs(cvol)))
    floor = max(CLEANUP_ABS_VOXELS * float(voxel) ** 3,
                CLEANUP_REL_VOLUME * largest)
    debris = (cfaces < CLEANUP_MIN_FACES) | (np.abs(cvol) < floor)

    in_min, in_max = V.min(axis=0), V.max(axis=0)
    tol = float(voxel)
    positive = cvol > 0.0
    keep = np.zeros(ncomp, bool)
    for i in range(ncomp):
        if debris[i]:
            continue
        if not (np.all(cmax[i] >= in_min - tol)
                and np.all(cmin[i] <= in_max + tol)):
            continue  # floating outside the input: not input material
        if positive[i]:
            keep[i] = True
        else:
            keep[i] = False  # inverted: only a nested, genuinely-void shell

    # inverted components: keep a true cavity (nested in a kept positive
    # component and the input is empty at its centre per the winding number)
    neg_ids = np.where((~positive) & (~debris))[0]
    kept_pos = np.where(keep)[0]
    if len(neg_ids) and len(kept_pos):
        winding = None
        try:
            bvh = sutura_geom.PyMeshBvh(v0.astype(np.float64),
                                        t0.astype(np.int64))
            winding = np.asarray(bvh.winding_points(ccent[neg_ids]),
                                 np.float64)
            info['input_winding'] = True
        except Exception:  # noqa: BLE001 - winding is an optional signal
            winding = None
        for k, i in enumerate(neg_ids):
            nested = False
            for p in kept_pos:
                if (np.all(cmin[i] >= cmin[p] - tol)
                        and np.all(cmax[i] <= cmax[p] + tol)):
                    nested = True
                    break
            if not nested:
                continue
            if winding is None or float(winding[k]) < 0.5:
                keep[i] = True

    info['kept_positive'] = int(np.count_nonzero(keep & positive))
    info['kept_cavities'] = int(np.count_nonzero(keep & ~positive))
    info['components_after'] = int(np.count_nonzero(keep))

    face_keep = keep[inv]
    if not np.any(face_keep):
        info['ran'] = False
        info['reason'] = 'cleanup would remove all geometry'
        return cv, ct, info
    if np.all(face_keep):
        return cv, ct, info

    dropped = ~keep
    info['removed_debris'] = int(np.count_nonzero(dropped & debris))
    info['removed_negative'] = int(np.count_nonzero(
        dropped & (~positive) & (~debris)))
    info['removed_other'] = int(np.count_nonzero(
        dropped & positive & (~debris)))
    info['removed_faces'] = int(cfaces[dropped].sum())
    info['removed_volume'] = float(np.abs(cvol[dropped]).sum())

    kept_tris = ct[face_keep]
    used = np.unique(kept_tris)
    remap = -np.ones(len(cv), np.int64)
    remap[used] = np.arange(len(used))
    return V[used], remap[kept_tris], info


# ---------------------------------------------------------------------------
# main entry point
# ---------------------------------------------------------------------------
def dressing_coat(verts, tris, *, voxel=None, intensity=None, grid_budget=None,
                  r_base=None, r_max=None, sigma=None, ml=None, guard=True,
                  time_budget=None, drain=None, defects=None,
                  r_max_scale=None, sigma_scale=None):
    """Dressing (#16) repair: extract the variable-viscosity level set.

    Returns ``(verts, tris, report)``.  The report always carries
    ``method=16``, ``display_name='Dressing'`` and ``invents_geometry=True``.
    The output is 2-manifold and self-intersection-free by construction; the
    fidelity gate is the coat->input deviation relative to the bbox diagonal.

    ``drain`` is the healthy-region erode-back.  ``None`` (the default) uses
    the preset's ``drain_factor`` (Balanced/Thorough full = ``r_base``, Quick
    off, Extreme deep = ``1.25*r_base``); it also accepts a mode name
    (``none``/``half``/``full``/``deep``) or an explicit ``delta_r`` in mm.  The
    drain is applied **in the level set** (``F = s - r + delta_r``), never as a
    vertex projection (which re-introduced 7,850 self-intersections and is
    permanently rejected).  A small residual outward bias (~0.4 voxel) remains
    even at full drain — the extractor's discretization, to be addressed in the
    Rust core, not hidden here.

    ``defects`` selects the mask that drives the viscosity field: ``'all'``
    (default) = holes + non-manifold + self-intersections, ``'holes_nm'`` =
    holes + non-manifold only.  ``r_max_scale`` / ``sigma_scale`` multiply the
    resolved ``r_max`` / ``sigma`` (``None`` = 1.0, the current default), so a
    narrower/tighter coat can be requested without new presets.
    """
    t_start = time.perf_counter()
    v0 = _as_f64(verts)
    t0 = _as_i64(tris)
    if intensity not in DRESSING_BY_INTENSITY:
        intensity = DRESSING_DEFAULT_INTENSITY
    ispec = DRESSING_BY_INTENSITY[intensity]
    if time_budget is None:
        time_budget = ispec['time_budget']
    report = {
        'method': METHOD_NUMBER,
        'display_name': DISPLAY_NAME,
        'display_name_full': DISPLAY_NAME_FULL,
        'invents_geometry': True,
        'ran': False,
        'ok': False,
        'category': 'error',
        'intensity': intensity,
        'engine': 'numpy',
        'voxel': None,
        'dims': [],
        'cells_evaluated': None,
        'r_base': None,
        'r_max': None,
        'sigma': None,
        'faces_before': int(len(t0)),
        'faces_coat': int(len(t0)),
        'faces_after': int(len(t0)),
        'decimated': False,
        'decimation': None,
        'si_before': None,
        'si_after': None,
        'si_exact_unknown': False,
        'holes_after': None,
        'nm_after': None,
        'hausdorff_rel_max': None,
        'hausdorff_rel_mean': None,
        'hausdorff_input_to_coat': None,
        'fidelity_ok': None,
        'normal_angle_p50': None,
        'normal_angle_p95': None,
        'normal_angle_p99': None,
        'normal_angle_max': None,
        'normal_angle_n': 0,
        'components_after': None,
        'volume_after': None,
        'drain': None,
        'defects': defects if defects in DEFECT_SETS else DEFECT_SET_DEFAULT,
        'r_max_scale': None,
        'sigma_scale': None,
        'within_time_budget': None,
        'time_budget_s': float(time_budget),
        'warnings': [],
        'seconds': 0.0,
    }
    if len(v0) == 0 or len(t0) == 0:
        report['error'] = 'empty mesh'
        report['seconds'] = round(time.perf_counter() - t_start, 3)
        return v0, t0, report

    diag = _diag(v0)
    ell = _median_edge_length(v0, t0)
    if grid_budget is None:
        grid_budget = DRESSING_GRID_BUDGET.get(intensity)
    if voxel is None:
        voxel = resolve_dressing_voxel(v0, t0, diag, intensity, grid_budget)
    voxel = float(voxel)
    report['si_before'] = _exact_si(v0, t0)
    defect_set = defects if defects in DEFECT_SETS else DEFECT_SET_DEFAULT
    rms = _normalize_scale(r_max_scale)
    sgs = _normalize_scale(sigma_scale)
    report['defects'] = defect_set
    report['r_max_scale'] = rms
    report['sigma_scale'] = sgs
    defect_idx, gap = _defect_vertex_indices(v0, t0, defects=defect_set)
    rb, rmx, sg = resolve_viscosity(ell, voxel, intensity, gap=gap,
                                    r_max_scale=rms, sigma_scale=sgs)
    if r_base is not None:
        rb = float(r_base)
    if r_max is not None:
        rmx = float(r_max)
    if sigma is not None:
        sg = float(sigma)

    # drain: healthy-region erode-back applied in the level set.  ``None`` uses
    # the preset default; a mode name or an explicit delta_r override it.
    drain_delta, drain_factor, drain_mode = _resolve_drain(drain, intensity)
    if drain_factor is not None:
        drain_delta = drain_factor * rb
    report['drain'] = {
        'mode': drain_mode,
        'factor': drain_factor,
        'delta_r': round(float(drain_delta), 6),
        'amount': round(float(drain_delta), 6),
        'drained': bool(drain_delta > 0.0),
    }

    # grid AABB: input bbox plus the bridging margin so the coat is closed
    margin = rmx + 3.0 * voxel
    box = np.vstack([v0.min(axis=0) - margin, v0.max(axis=0) + margin])

    cv = ct = None
    meta = {}
    rust = _rust_dressing_coat(v0, t0, voxel, rb, rmx, sg, defect_idx, box,
                               drain_delta)
    if rust is not None:
        cv, ct, meta = rust
        report['engine'] = 'rust'
    if cv is None or len(ct) == 0:
        cv, ct, meta = _numpy_coat(v0, t0, voxel, rb, rmx, sg, defect_idx, box,
                                   drain_delta)
        report['engine'] = 'numpy'
    if len(ct) == 0:
        report['error'] = 'level-set extraction produced no geometry'
        report['seconds'] = round(time.perf_counter() - t_start, 3)
        return v0, t0, report
    ct = _reorient_faces(cv, ct)
    report['ran'] = True

    # actual (possibly coarsened) resolution for the report.  The Rust core
    # also reports its narrow-band stats / timings / manifold verdict; surface
    # them when present (numpy path leaves them None).
    voxel = float(meta.get('voxel', voxel))
    report.update({
        'voxel': voxel,
        'dims': list(meta.get('dims', [])),
        'cells_evaluated': meta.get('voxels'),
        'grid_coarsened': bool(meta.get('coarsened', False)),
        'r_base': round(float(meta.get('r_base', rb)), 6),
        'r_max': round(float(meta.get('r_max', rmx)), 6),
        'sigma': round(float(meta.get('sigma', sg)), 6),
        'band': meta.get('band'),
        'band_cells': meta.get('band_cells'),
        'field_seconds': meta.get('field_seconds'),
        'extract_seconds': meta.get('extract_seconds'),
        'rust_manifold': meta.get('manifold'),
        'faces_coat': int(len(ct)),
    })

    # post-coat component cleanup: drop extraction debris / stray inverted
    # shells before decimation (a no-op for a single-component coat)
    cv, ct, cleanup = _clean_coat_components(v0, t0, cv, ct, voxel)
    report['cleanup'] = cleanup
    report['faces_coat_clean'] = int(len(ct))

    # decimation toward the input face budget, with rollback
    target = _target_faces(len(t0))
    dv, dt, drec = _decimate_coat(ml, cv, ct, target)
    report['decimation'] = drec
    report['decimated'] = bool(drec.get('decimated'))
    cv, ct = dv, dt

    # reload-safe Stitch (P-WELD) + honest verdict
    wv, wt, pw = _p_weld(cv, ct, ml)
    h, nm = reload_strict_holes_nm(wv, wt)
    report['p_weld'] = pw
    report['holes_after'] = int(h)
    report['nm_after'] = int(nm)
    report['si_after'] = _exact_si(wv, wt)
    report['si_exact_unknown'] = report['si_after'] is None
    report['faces_after'] = int(len(wt))

    # normal-direction fidelity (voxel staircase / fluting detector) and the
    # global shape guards the adoption gate uses (component count, volume)
    na = normal_angle_deviation(wv, wt, v0, t0, defect_idx)
    if na is not None:
        report['normal_angle_p50'] = round(na['p50'], 4)
        report['normal_angle_p95'] = round(na['p95'], 4)
        report['normal_angle_p99'] = round(na['p99'], 4)
        report['normal_angle_max'] = round(na['max'], 4)
        report['normal_angle_n'] = int(na['n'])
    report['components_after'] = count_components(wt)
    report['volume_after'] = round(signed_volume(wv, wt), 6)

    # fidelity: every coat point near the input surface (the prototype's
    # fidelity proof); the input->coat direction is informational (a bridging
    # coat intentionally does not reproduce the damaged surface)
    if guard and diag > 0.0:
        hmax, hmean = _one_sided_hausdorff(wv, wt, v0, t0, diag)
        report['hausdorff_rel_max'] = hmax
        report['hausdorff_rel_mean'] = hmean
        report['hausdorff_input_to_coat'] = _one_sided_hausdorff(
            v0, t0, wv, wt, diag)[0]
    measured = report['hausdorff_rel_max']
    fidelity_ok = (not guard) or (measured is None) or (
        measured <= ispec['max_hausdorff'])
    report['fidelity_ok'] = bool(fidelity_ok)
    report['seconds'] = round(time.perf_counter() - t_start, 3)
    report['within_time_budget'] = bool(report['seconds'] <= float(time_budget))

    watertight = (h == 0 and nm == 0)
    si_ok = (report['si_after'] in (0, None))
    report['ok'] = bool(watertight and si_ok and fidelity_ok)
    report['category'] = 'watertight' if report['ok'] else 'partial'

    if measured is not None and not fidelity_ok:
        report['warnings'].append({
            'code': 'dressing_detail_loss',
            'message_en': (
                'Dressing used a %.3f mm viscosity radius: the coat deviates '
                '%.2f%% of the diagonal from the input surface (fine detail '
                'may be smoothed).' % (rb, 100.0 * measured)),
            'message_tr': (
                'Dressing %.3f mm viskozite yarıçapı kullandı: kaplama girdi '
                'yüzeyinden çaprazın %%%.2f kadar sapıyor (ince detay '
                'yumuşamış olabilir).' % (rb, 100.0 * measured)),
        })
    if not report['within_time_budget']:
        report['warnings'].append({
            'code': 'dressing_time',
            'message_en': (
                'Dressing took %.1f s, over the %s preset budget of %.0f s.'
                % (report['seconds'], intensity, float(time_budget))),
            'message_tr': (
                'Dressing %.1f sn sürdü; %s ön ayarının %.0f sn bütçesini '
                'aştı.' % (report['seconds'], intensity, float(time_budget))),
        })
    return _as_f64(wv), _as_i64(wt), report


if __name__ == '__main__':
    import sys
    import json
    import trimesh
    path = sys.argv[1]
    _m = trimesh.load(path, force="mesh")
    ov, ot, rep = dressing_coat(np.asarray(_m.vertices, np.float64),
                                np.asarray(_m.faces, np.int64))
    print(json.dumps(rep, indent=2, default=str))
