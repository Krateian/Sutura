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
                  time_budget=None) -> (verts_out, tris_out, report)

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


def resolve_viscosity(ell_median, voxel, intensity=None, gap=None):
    """``(r_base, r_max, sigma)`` for one mesh.

    Thin base radius over healthy detailed surface (the fidelity mechanism;
    framebaroque r_base ~= 0.17 mm -> 0.06 % of the diagonal), a preset-capped
    bridging radius near defects (also capped at 45 % of the largest gap, so a
    coat over a big opening cannot balloon past the model), and a transition
    width tied to the feature size / voxel.
    """
    if intensity not in DRESSING_BY_INTENSITY:
        intensity = DRESSING_DEFAULT_INTENSITY
    r_preset = DRESSING_BY_INTENSITY[intensity]['r_preset']
    r_base = float(np.clip(0.3 * ell_median, 0.25 * voxel, 0.8 * voxel))
    r_max = float(min(r_preset, 3.5 * ell_median)) if ell_median > 0 else r_preset
    if gap is not None and gap > 0:
        r_max = min(r_max, 0.45 * float(gap))
    r_max = max(r_max, r_base)
    sigma = float(max(2.0 * ell_median, 2.0 * voxel))
    return r_base, r_max, sigma


def _defect_vertex_indices(v, t):
    """``(vertex indices, gap)`` touched by holes, non-manifold regions and
    (when within the Rust cap) self-intersecting faces.  ``gap`` is the largest
    hole diameter, used to cap the bridging radius."""
    idx = set()
    gap = 0.0
    det = detect_defects(v, t, with_indices=True)
    for h in det['holes']:
        idx.update(int(i) for i in h.get('verts_idx', ()))
        gap = max(gap, float(h.get('diameter') or 0.0))
    for nm in det['non_manifold']:
        idx.update(int(i) for i in nm.get('verts_idx', ()))
    if len(t) <= SI_MAX_FACES:
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
    """Radius grid from the EDT of the defect mask (scipy, lazy import)."""
    from scipy import ndimage
    dx, dy, dz = int(dims[0]), int(dims[1]), int(dims[2])
    if len(defect_idx) == 0:
        return np.full((dx, dy, dz), np.float32(r_base))
    dvx = (verts[defect_idx] - origin) / voxel
    gi = np.clip(np.round(dvx[:, 0]).astype(np.int64), 0, dx - 1)
    gj = np.clip(np.round(dvx[:, 1]).astype(np.int64), 0, dy - 1)
    gk = np.clip(np.round(dvx[:, 2]).astype(np.int64), 0, dz - 1)
    mask = np.zeros((dx, dy, dz), dtype=bool)
    mask[gi, gj, gk] = True
    d_defect = ndimage.distance_transform_edt(~mask).astype(np.float32) * np.float32(voxel)
    return (r_base + (r_max - r_base)
            * np.exp(-0.5 * (d_defect / sigma) ** 2)).astype(np.float32)


# ---------------------------------------------------------------------------
# extraction (Rust when available, numpy prototype otherwise)
# ---------------------------------------------------------------------------
def _rust_dressing_coat(v, t, voxel, r_base, r_max, sigma, defect_idx, box,
                        drain_delta=0.0):
    """Delegate to ``sutura_geom.dressing_coat`` when the installed extension
    provides it.  Returns ``(verts, tris, info)`` or ``None``.

    The Rust binding is still being built; any signature mismatch or runtime
    error falls back to the numpy path (never raises)."""
    fn = getattr(sutura_geom, 'dressing_coat', None)
    if fn is None:
        return None
    try:
        out = fn(v, t, voxel=voxel, r_base=r_base, r_max=r_max, sigma=sigma,
                 defect_verts=defect_idx, box=box, drain=drain_delta)
    except TypeError:
        try:
            out = fn(v, t, voxel, r_base, r_max, sigma, defect_idx, box)
        except Exception:  # noqa: BLE001
            return None
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

    ``drain_delta`` is added to the level set (``F = s - r + drain_delta``): a
    positive value erodes the coat back toward the original surface on healthy
    regions (where ``r ~ r_base``), while defect regions stay covered because
    their ``r`` is much larger (offset report §1)."""
    w, u, s, info = sutura_geom.sdf_grid(v, t, voxel=voxel, box=box)
    voxel = float(info['voxel'])
    origin = np.asarray(info['origin'], dtype=np.float64)
    dims = tuple(int(x) for x in info['dims'])
    r_grid = _variable_radius(dims, origin, voxel, defect_idx, v,
                              r_base, r_max, sigma)
    F = np.asarray(s, dtype=np.float32) - r_grid
    if drain_delta:
        F += np.float32(drain_delta)
    del s, r_grid
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
# main entry point
# ---------------------------------------------------------------------------
def dressing_coat(verts, tris, *, voxel=None, intensity=None, grid_budget=None,
                  r_base=None, r_max=None, sigma=None, ml=None, guard=True,
                  time_budget=None, drain=None):
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
        'drain': None,
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
    defect_idx, gap = _defect_vertex_indices(v0, t0)
    rb, rmx, sg = resolve_viscosity(ell, voxel, intensity, gap=gap)
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

    # actual (possibly coarsened) resolution for the report
    voxel = float(meta.get('voxel', voxel))
    report.update({
        'voxel': voxel,
        'dims': list(meta.get('dims', [])),
        'cells_evaluated': meta.get('voxels'),
        'grid_coarsened': bool(meta.get('coarsened', False)),
        'r_base': round(rb, 6),
        'r_max': round(rmx, 6),
        'sigma': round(sg, 6),
        'faces_coat': int(len(ct)),
    })

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
