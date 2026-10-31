# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Tamponade interior plugging tier (P1a split).

Moved verbatim from sutura/repair.py; no logic change.
"""
import math
import os

import numpy as np

import topology
from defects import detect as detect_defects
from sutura_engine import health as method_health
from sutura_engine.pipeline.mesh_io import stl_write_binary
from sutura_engine.pipeline.p_weld import (
    reload_strict_holes_nm, weld_reload_equivalent,
)
from sutura_engine.pipeline.tiers.local_remesh import _local_si_rise
from sutura_engine.pipeline.utils import (
    _bbox_diagonal, _count_vertex_components, _exact_si_face_count,
    _internal_wall_count, signed_volume, surface_area,
)
from sutura_engine.policy import TIERS as _TIERS

_TAMP_POLICY = _TIERS['tamponade']
_TAMP_GATES = _TAMP_POLICY['gates']


# Tamponade final safety net (G11/G12 release blocker). The Tamponade adoption
# gate is decided on the pre-chain / stage-1 mesh and only proves that damage
# (holes + non-manifold) falls; it says nothing about the FINAL shape. On
# thingi10k_100827 a Tamponade prepass adoption (holes 6 -> 4 on a 71-face
# scan) let the downstream deep tier rebuild the 37 mm object into a 7 mm
# fragment (G11: bbox-collapse trigger), and even after the Graft coverage gate
# (G12) stopped the fragment the Tamponade path left a 1.83 mm outside-the-input
# leak the no-tamponade path did not have (leak trigger). repair_file re-runs
# once with Tamponade OFF and keeps the candidate that covers more of the input
# and does not leak further outside it (never worse on damage or part count).
TAMP_COLLAPSE_BBOX_FRAC = float(_TAMP_GATES['collapse_bbox_frac'])


# Outside-the-input leak trigger: a Tamponade-adopted result whose added
# vertices lie outside the input solid beyond this fraction of the input
# diagonal re-runs Tamponade OFF. 1 % is above the pre-existing (non-Tamponade)
# leak of heavily folded meshes (ornate-frame 0.09 %) but far below the
# 100827 Tamponade leak (4.9 %).
TAMP_LEAK_TRIGGER_FRAC = float(_TAMP_GATES['leak_trigger_frac'])


# Above this face count the leak metric is skipped (its input-face key set +
# BVH would dominate the run) and only the cheap bbox trigger applies.
TAMP_LEAK_MAX_FACES = int(_TAMP_POLICY['leak_max_faces'])


# Shape suitability gate (v1). A tier-level gate on existing fields: a model
# with no real interior (open sheet / thin shell) has nothing for the inside-out
# plug to fill and is left to Flap. Compactness = 36*pi*V^2/A^3 (sphere 1, cube
# 0.52, sheet ~0); open_area_ratio is the diagnosed boundary-loop area / surface
# area; thin_sample_fraction is the existing diagnosis_v2 thin_wall ratio.
# All three must agree, so no single field can gate on its own. interior_depth
# is reported from the melt DepthField but is not yet a hard gate until the
# calibration run proves a safe threshold.
TAMP_SHAPE_COMPACTNESS = float(_TAMP_GATES['shape_compactness'])
TAMP_SHAPE_OPEN_AREA = float(_TAMP_GATES['shape_open_area'])
TAMP_SHAPE_THIN = float(_TAMP_GATES['shape_thin'])
TAMP_SHAPE_THICKNESS = float(_TAMP_GATES['shape_thickness'])
TAMP_SHAPE_DEPTH_CELLS = int(_TAMP_GATES['shape_depth_cells'])
TAMP_SHAPE_PERIMETER_RATIO = float(_TAMP_GATES['shape_perimeter_ratio'])
TAMP_SHAPE_SOLID_AREA = float(_TAMP_GATES['shape_solid_area'])
TAMP_SHAPE_WINDING = float(_TAMP_GATES['shape_winding'])
TAMP_SHAPE_SIDE_SAMPLES = int(_TAMP_GATES['shape_side_samples'])
TAMP_SHAPE_SIDE_EPS = float(_TAMP_GATES['shape_side_eps'])


def _shape_gate_enabled():
    return os.environ.get('SUTURA_TAMPONADE_SHAPE_GATE', '1') != '0'


def _loop_spanned_area(verts, loop_idx):
    idx = np.asarray(loop_idx, dtype=np.int64)
    if len(idx) < 3:
        return 0.0
    p = np.asarray(verts, dtype=np.float64)[idx]
    q = np.roll(p, -1, axis=0)
    nrm = np.array([
        np.sum((p[:, 1] - q[:, 1]) * (p[:, 2] + q[:, 2])),
        np.sum((p[:, 2] - q[:, 2]) * (p[:, 0] + q[:, 0])),
        np.sum((p[:, 0] - q[:, 0]) * (p[:, 1] + q[:, 1])),
    ])
    return float(0.5 * np.linalg.norm(nrm))


def _boundary_perimeter(v, t):
    """Total length of boundary edges (edges used by exactly one face)."""
    tt = np.asarray(t, dtype=np.int64)
    if len(tt) == 0:
        return 0.0
    V = int(tt.max()) + 1
    f0, f1, f2 = tt[:, 0], tt[:, 1], tt[:, 2]
    keys = np.concatenate([
        np.minimum(f0, f1) * V + np.maximum(f0, f1),
        np.minimum(f1, f2) * V + np.maximum(f1, f2),
        np.minimum(f0, f2) * V + np.maximum(f0, f2),
    ])
    uniq, counts = np.unique(keys, return_counts=True)
    bkeys = uniq[counts == 1]
    if len(bkeys) == 0:
        return 0.0
    ba, bb = bkeys // V, bkeys % V
    vv = np.asarray(v, dtype=np.float64)
    return float(np.linalg.norm(vv[ba] - vv[bb], axis=1).sum())


def _solid_area_fraction(v, t):
    """Area fraction whose two face sides reach a solid winding (|w| > 0.9).

    Open shells have winding magnitudes around 0.5-0.8 on both sides; a solid
    has one side near 1. The fraction is area-weighted and sampled
    deterministically so tessellation and face count do not move the threshold.
    Returns ``None`` when the extension or a valid normal is unavailable.
    """
    try:
        import sutura_geom
        if not hasattr(sutura_geom, 'PyMeshBvh'):
            return None
        vv = np.asarray(v, dtype=np.float64)
        tt = np.asarray(t, dtype=np.int32)
        if len(vv) == 0 or len(tt) == 0:
            return None
        if len(tt) > TAMP_SHAPE_SIDE_SAMPLES:
            sel = np.unique(np.linspace(
                0, len(tt) - 1, TAMP_SHAPE_SIDE_SAMPLES).astype(np.int64))
            ft = tt[sel]
        else:
            ft = tt
        a, b, c = vv[ft[:, 0]], vv[ft[:, 1]], vv[ft[:, 2]]
        cross = np.cross(b - a, c - a)
        lengths = np.linalg.norm(cross, axis=1)
        areas = 0.5 * lengths
        total = float(areas.sum())
        if total <= 0.0:
            return None
        normals = np.zeros_like(cross)
        good = lengths > 1e-12
        normals[good] = cross[good] / lengths[good, None]
        diag = _bbox_diagonal(vv)
        eps = diag * TAMP_SHAPE_SIDE_EPS
        if not (eps > 0.0):
            return None
        centers = (a + b + c) / 3.0
        pts = np.vstack([centers + normals * eps,
                         centers - normals * eps])
        bvh = sutura_geom.PyMeshBvh(vv, tt)
        w = np.asarray(bvh.winding_points(pts), dtype=np.float64)
        n = len(ft)
        mx = np.maximum(np.abs(w[:n]), np.abs(w[n:]))
        return float(areas[mx > TAMP_SHAPE_WINDING].sum() / total)
    except Exception:
        return None


def _thin_sample_fraction(v, t):
    try:
        import sutura_geom
        if not hasattr(sutura_geom, 'diagnose'):
            return None
        diag = sutura_geom.diagnose(
            np.asarray(v, dtype=np.float32), np.asarray(t, dtype=np.int32),
            detail='fast', masks=False)
        tw = (diag or {}).get('thin_wall') or {}
        if not tw.get('available'):
            return None
        return float(tw.get('thin_ratio') or 0.0)
    except Exception:
        return None


def _interior_depth_norm(v, t):
    try:
        import sutura_geom
        fn = getattr(sutura_geom, 'interior_depth', None)
        if fn is None:
            return None
        out = fn(np.asarray(v, dtype=np.float64),
                 np.asarray(t, dtype=np.int32), TAMP_SHAPE_DEPTH_CELLS)
        return None if out is None else float(out)
    except Exception:
        return None


def _tamponade_shape(v, t, holes, after=None):
    """Shape suitability card for Tamponade (cheap; depth is report-only)."""
    area = surface_area(v, t)
    vol = abs(signed_volume(v, t))
    compactness = (36.0 * math.pi * vol * vol / (area ** 3)
                   if area > 0.0 else 0.0)
    open_area = sum(_loop_spanned_area(v, h.get('verts_idx') or [])
                    for h in (holes or []))
    open_ratio = open_area / area if area > 0.0 else 0.0
    genus = None
    try:
        if isinstance(after, dict):
            genus = after.get('genus')
        if genus is None:
            genus = topology.mesh_topology(v, t).get('genus')
    except Exception:
        genus = None
    diag = _bbox_diagonal(v)
    thickness = (2.0 * vol / area / diag
                 if area > 0.0 and diag > 0.0 else 0.0)
    thin = None
    if compactness <= TAMP_SHAPE_COMPACTNESS and open_ratio >= TAMP_SHAPE_OPEN_AREA:
        thin = _thin_sample_fraction(v, t)
    thin_shell = (compactness <= TAMP_SHAPE_COMPACTNESS
                  and open_ratio >= TAMP_SHAPE_OPEN_AREA
                  and thin is not None and thin >= TAMP_SHAPE_THIN)
    no_volume = (thickness <= TAMP_SHAPE_THICKNESS
                 and open_ratio >= TAMP_SHAPE_OPEN_AREA)
    perimeter = _boundary_perimeter(v, t)
    perimeter_ratio = (perimeter / math.sqrt(area)
                       if area > 0.0 else 0.0)
    solid_area = None
    if (not thin_shell and not no_volume
            and perimeter_ratio >= TAMP_SHAPE_PERIMETER_RATIO):
        solid_area = _solid_area_fraction(v, t)
    open_shell = (perimeter_ratio >= TAMP_SHAPE_PERIMETER_RATIO
                  and solid_area is not None
                  and solid_area <= TAMP_SHAPE_SOLID_AREA)
    return {
        'genus': None if genus is None else int(genus),
        'compactness': round(float(compactness), 6),
        'open_area_ratio': round(float(open_ratio), 6),
        'effective_thickness_norm': round(float(thickness), 6),
        'thin_sample_fraction': (None if thin is None else round(float(thin), 6)),
        'interior_depth_norm': _interior_depth_norm(v, t),
        'boundary_perimeter': round(float(perimeter), 6),
        'perimeter_ratio': round(float(perimeter_ratio), 6),
        'solid_area_fraction': (None if solid_area is None
                                else round(float(solid_area), 6)),
        'thresholds': {
            'compactness': TAMP_SHAPE_COMPACTNESS,
            'open_area_ratio': TAMP_SHAPE_OPEN_AREA,
            'thin_sample_fraction': TAMP_SHAPE_THIN,
            'effective_thickness_norm': TAMP_SHAPE_THICKNESS,
            'perimeter_ratio': TAMP_SHAPE_PERIMETER_RATIO,
            'solid_area_fraction': TAMP_SHAPE_SOLID_AREA,
        },
        'skip': bool(thin_shell or no_volume or open_shell),
        'reason': ('thin_shell' if thin_shell else
                   ('no_volume' if no_volume else
                    ('open_shell' if open_shell else None))),
    }


def _tamponade_shape_leak(in_verts, in_tris, out_verts, out_tris, in_diag):
    """Outside-the-input leak of a repaired mesh, mirroring the A18 harness.

    The result's ADDED vertices (those not on any input face) are tested with
    the generalized winding number against the input solid: a vertex OUTSIDE the
    input (``w < 0.5``) and further than the shape tolerance from it is a leak
    (material bulging into empty space, e.g. thingi10k_100827's 1.83 mm leak).
    Returns ``(leak_mm, count)``; ``(0.0, 0)`` when the mesh is empty, too large
    to test, or the extension is unavailable.
    """
    try:
        import sutura_geom
        iv = np.asarray(in_verts, dtype=np.float64)
        it = np.asarray(in_tris, dtype=np.int32)
        ov = np.asarray(out_verts, dtype=np.float64)
        ot = np.asarray(out_tris, dtype=np.int32)
        if (len(it) == 0 or len(ot) == 0
                or len(it) > TAMP_LEAK_MAX_FACES):
            return 0.0, 0
        in_keys = set(map(tuple, np.sort(it, axis=1).tolist()))
        add_mask = np.fromiter(
            (tuple(sorted(row)) not in in_keys for row in ot.tolist()),
            dtype=bool, count=len(ot))
        if not add_mask.any():
            return 0.0, 0
        add_v = ov[np.unique(ot[add_mask])]
        # The cheap closest-point pass runs first and cheaply answers "is any
        # added vertex farther than the tolerance from the input?"; only the
        # far vertices can ever be a leak, so the winding BVH (the expensive
        # part) is built and queried on that subset alone. Identical result: a
        # vertex with ``dist <= tol`` can never satisfy ``bad``.
        _q, dist, _f = sutura_geom.closest_points(iv, it, add_v)
        dist = np.asarray(dist, dtype=np.float64)
        tol = min(max(in_diag / 10000.0, 0.0125), 0.05)
        far = dist > tol
        if not far.any():
            return 0.0, 0
        far_v = add_v[far]
        far_dist = dist[far]
        bvh = sutura_geom.PyMeshBvh(iv, it)
        w_classic = np.asarray(bvh.winding_points(far_v))
        bad = w_classic < 0.5
        if (sutura_geom.__dict__.get("ANTIPODAL_SUPPORTED", False)
                and os.environ.get("SUTURA_ANTIPODAL", "").strip().lower()
                not in ("0", "false", "no", "off")):
            # Conservative admission gate: admit a plug only if the leak check
            # passes under BOTH oracles (classic and antipodal). If either flags
            # an outside-the-input vertex, treat it as a leak.
            _inside, _w = sutura_geom.antipodal_points(iv, it, far_v)
            w_antipodal = np.asarray(_w, dtype=np.float64)
            bad = bad | (w_antipodal < 0.5)
        if not bad.any():
            return 0.0, 0
        return float(far_dist[bad].max()), int(bad.sum())
    except Exception:  # noqa: BLE001 - a metric failure is not fatal
        return 0.0, 0


def _tamponade_final_needs_off(in_verts, in_tris, out_verts, out_tris,
                               force_metrics=False):
    """Whether a Tamponade-adopted FINAL mesh warrants a Tamponade-OFF re-run.

    Returns ``(reason, metrics)`` with ``reason`` None when the result is
    acceptable. Two cheap triggers: a bounding-box collapse relative to the
    input (``coverage < TAMP_COLLAPSE_BBOX_FRAC``, a fragment far smaller than
    the object) and an outside-the-input leak above ``TAMP_LEAK_TRIGGER_FRAC``
    of the diagonal. ``metrics`` carries the reload-honest damage, the
    component count and the leak so the OFF candidate can be compared.

    The damage metrics (holes / non-manifold / components) are only consumed
    when a trigger fires and the OFF comparison runs, so on the common
    no-trigger path they are skipped and ``(None, None)`` is returned. The
    OFF-side call sets ``force_metrics=True`` so its metrics dict is always
    built (the comparison reads it unconditionally).
    """
    in_diag = _bbox_diagonal(in_verts)
    out_diag = _bbox_diagonal(out_verts)
    coverage = (out_diag / in_diag) if in_diag else 1.0
    leak_mm, leak_pts = _tamponade_shape_leak(
        in_verts, in_tris, out_verts, out_tris, in_diag)
    reason = None
    if in_diag and coverage < TAMP_COLLAPSE_BBOX_FRAC:
        reason = 'bbox_collapse'
    elif in_diag and (leak_mm / in_diag) > TAMP_LEAK_TRIGGER_FRAC:
        reason = 'shape_leak'
    if reason is None and not force_metrics:
        return None, None
    try:
        holes, nm = reload_strict_holes_nm(out_verts, out_tris)
        wv, wt = weld_reload_equivalent(out_verts, out_tris)
        parts = _count_vertex_components(len(wv), wt)
    except Exception:  # noqa: BLE001 - metrics are best-effort
        holes = nm = parts = None
    metrics = {'input_diag_mm': round(in_diag, 4),
               'output_diag_mm': round(out_diag, 4),
               'coverage': round(coverage, 4),
               'leak_mm': round(leak_mm, 4), 'leak_pts': leak_pts,
               'holes': holes, 'non_manifold': nm, 'parts': parts}
    return reason, metrics


def _tamponade_off_preferred(on_metrics, off_metrics):
    """Whether the Tamponade-OFF result should replace the Tamponade-ON one.

    OFF wins only when it is never worse on damage or component count and then
    leaks strictly less outside the input or covers strictly more of its
    bounding box; a tie keeps the Tamponade result, so an adoption is discarded
    only when it is actually worse. Unknown metrics (``None``) are treated
    conservatively as "not worse".
    """
    def _damage(m):
        h = m.get('holes')
        n = m.get('non_manifold')
        if h is None or n is None:
            return None
        return h + n

    def _parts(m):
        return m.get('parts')

    on_dmg, off_dmg = _damage(on_metrics), _damage(off_metrics)
    if on_dmg is not None and off_dmg is not None and off_dmg > on_dmg:
        return False
    on_parts, off_parts = _parts(on_metrics), _parts(off_metrics)
    if on_parts is not None and off_parts is not None and off_parts > on_parts:
        return False
    if off_metrics.get('leak_mm', 0.0) < on_metrics.get('leak_mm', 0.0):
        return True
    return off_metrics.get('coverage', 0.0) > on_metrics.get('coverage', 0.0)


def _dump_tamp_mesh(dump_dir, name, verts, tris):
    """Write one Tamponade diagnostic mesh when ``SUTURA_TAMP_DUMP`` is set.

    The variable holds a directory; a binary STL (the on-disk reload form) and
    an ``.npz`` with the exact in-memory arrays are written so the tier's input
    and output can be reloaded and scored independently. Never raises: a dump
    failure must not affect a repair.
    """
    try:
        if not dump_dir:
            return
        os.makedirs(dump_dir, exist_ok=True)
        stl_write_binary(os.path.join(dump_dir, name + '.stl'), verts, tris)
        np.savez(os.path.join(dump_dir, name + '.npz'),
                 verts=np.asarray(verts, dtype=np.float64),
                 tris=np.asarray(tris, dtype=np.int64))
    except Exception:  # noqa: BLE001 - diagnostics never break a repair
        pass


from sutura_engine.pipeline.adopt_guard import check_candidate, policy_for


def tamponade_tier(v, t, input_v=None, input_t=None, after=None, stats=None,
                   tmpdir=None, tamponade=False, intensity=None, spec=None,
                   si_mode='report', prepass=False, tamponade_force=False):
    """Interior-plug tier for registry method 17 (Tamponade).

    Runs ``sutura_engine.tamponade.run_tamponade`` on the CURRENT stage-1 result
    (after Stage 1 and the Flap pre-pass, before Graft/fTetWild).  The candidate
    is adopted only when, on the reload-honest (welded) mesh, neither holes nor
    non-manifold edges rise, the two-total strictly falls, the component count
    does not rise and at least 99 % of the pre-tier faces are carried through
    verbatim (``healthy_faces_kept``).  ``stats['tamponade']`` records the
    verdict and the Rust counters.  Never raises.

    ``prepass=True`` is the T4 placement where Tamponade runs on the CLEANED
    input BEFORE the Stage-1 chain, so the chain's own cleanup + close_holes
    steps repair the non-manifold edge the reference plug can leave behind and
    re-cap any small loop it opens.  The gate is therefore relaxed: the
    candidate must strictly reduce the reload-honest HOLES (the seal worked)
    and keep >= 99 % of the input faces; a non-manifold rise is tolerated
    (Stage 1 fixes it).  ``rec['placement']`` records which placement ran.
    """
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    if not tamponade:
        return v, t, after
    rec = {'tier': 'tamponade', 'ran': False, 'adopted': False,
           'placement': 'prepass' if prepass else 'post_stage1',
           'watertight': False, 'reload_watertight': False, 'reason': None,
           'reject_reason': None, 'intensity': intensity,
           'work_budget': None, 'work_units': None,
           'h_min': None, 'input_faces': None, 'output_faces': None,
           'seconds': None, 'input_holes': None, 'input_non_manifold': None,
           'input_parts': None, 'candidate_holes': None,
           'candidate_non_manifold': None, 'candidate_parts': None,
           'plugs': None, 'plug_faces': None, 'plugs_rejected': None,
           'plugs_inconclusive': None, 'plugs_rejected_size': None,
           'plugs_rejected_dev': None, 'melt_layers_tried': None,
           'melt_layers_accepted': None, 'melt_layers_rolled_back': None,
           'micro_welds_ok': None, 'micro_welds_failed': None,
           'excised_faces': None,
            'healthy_faces_kept': None, 'healthy_fraction': None,
            'plugs_rolled_back': None, 'plugs_leaking': None,
            'plugs_double_wall': None, 'plugs_floating': None,
            'components_before': None, 'components_after': None,
            'closed_shells_before': None, 'closed_shells_after': None,
           'input_self_intersections': None,
           'candidate_self_intersections': None, 'si_fall': None,
           'si_ok': None, 'melt_enabled': None, 'si_method': None,
           'input_internal_walls': None, 'candidate_internal_walls': None,
           'prepass_seconds': None,
           'guard_rejected': None, 'guard_reason': None,
           'baseline_self_intersections': None,
           'candidate_self_intersections': None,
           'si_policy': None, 'si_checked': False, 'si_flagged': False,
           'gate': None, 'shape': None, 'reason_key': None}
    if 'tamponade_shell_prepass' in stats:
        rec['shell_prepass'] = stats['tamponade_shell_prepass']
    with method_health.tier_guard('tamponade', 'tier') as _g:
        cur_v = v
        cur_t = t
        dump_dir = os.environ.get('SUTURA_TAMP_DUMP')
        _dump_tamp_mesh(dump_dir, 'tamp_input', cur_v, cur_t)
        rec['input_faces'] = int(len(cur_t))
        # Base metrics MUST use the same reload-honest definition as the
        # candidate below (weld_reload_equivalent + defects.detect), otherwise
        # the gate compares in-memory boundary loops / pymeshlab non-manifold
        # edges against the candidate's on-reload values and a true no-op can
        # look like a regression (ornate-frame: boundary_loop_stats 39 vs the
        # reload-honest 23).  `after` carries the in-memory measures only.
        wb_v, wb_t = weld_reload_equivalent(cur_v, cur_t)
        _base_defects = detect_defects(wb_v, wb_t, with_indices=True)
        base_holes = len(_base_defects['holes'])
        base_nm = len(_base_defects['non_manifold'])
        base_parts = _count_vertex_components(len(wb_v), wb_t)
        rec['input_holes'] = base_holes
        rec['input_non_manifold'] = base_nm
        rec['input_parts'] = base_parts

        try:
            from sutura_engine import tamponade as _tamp
        except Exception:  # noqa: BLE001
            import tamponade as _tamp
        intensity_val = (intensity
                         or getattr(spec, 'base', None)
                         or getattr(spec, 'name', None))
        # The SI melt is opt-in by intensity (thorough/extreme, or forced by
        # SUTURA_TAMPONADE_MELT=1); at quick/balanced only the hole plug runs.
        melt_on = bool(_tamp.resolve_melt_si(intensity_val))
        rec['melt_enabled'] = melt_on

        # Prepass budget: the seal is capped by a deterministic cut-grid-cell
        # work budget (no wall clock, no elapsed subtraction). A cap <= 0 skips
        # the prepass outright; the local SI guard runs after the seal.
        prepass_budget = None
        if prepass:
            prepass_budget = _tamp.resolve_tamponade_prepass_budget(intensity_val)
            if prepass_budget is not None and prepass_budget <= 0:
                rec['reason'] = ('prepass work budget 0 at intensity %r; tier '
                                 'skipped' % (intensity_val,))
                stats['tamponade'] = rec
                return v, t, after

        # One exact SI scan of the pre-tier mesh, reused for the melt gate, the
        # skip decision and the adoption guard.  It is skipped when si_mode is
        # 'off', and on a closed mesh at quick/balanced (nothing to plug, melt
        # off) no scan is needed at all.  The prepass uses the cheap LOCAL SI
        # guard instead (see below), so it never pays for this full scan.
        # Measured on the WELDED base (`wb_v`/`wb_t`) so it matches the
        # candidate's `wv`/`wt` scan exactly: `_exact_si_face_count` is
        # representation-sensitive (float32 welding merges near-coincident
        # vertices), so a no-op candidate read against the raw float64 input
        # otherwise shows a spurious SI fall and gets adopted for nothing.
        base_si_exact = None
        if (not prepass and si_mode != 'off'
                and (melt_on or base_holes > 0 or base_nm > 0)):
            base_si_exact = _exact_si_face_count(wb_v, wb_t)
        base_si = base_si_exact if base_si_exact is not None else 0
        rec['input_self_intersections'] = base_si_exact

        if base_holes == 0 and base_nm == 0 and not melt_on:
            rec['reason'] = ('no holes/non-manifold to plug; melt disabled at '
                             'this intensity; tier skipped')
            stats['tamponade'] = rec
            return v, t, after
        if (not prepass and base_holes == 0 and base_nm == 0
                and base_si == 0 and not base_si_exact):
            rec['reason'] = ('no holes/non-manifold/self-intersections to plug; '
                             'tier skipped')
            stats['tamponade'] = rec
            return v, t, after

        shape = _tamponade_shape(wb_v, wb_t, _base_defects.get('holes'), after)
        rec['shape'] = shape
        if not tamponade_force and _shape_gate_enabled() and shape['skip']:
            rec['gate'] = 'shape_card'
            _skip_reason = shape.get('reason')
            if _skip_reason == 'no_volume':
                rec['reason_key'] = 'tamponade_skip_no_volume'
                rec['reason'] = ('not suitable: no interior volume (open sheet); '
                                 'left to Flap')
            elif _skip_reason == 'open_shell':
                rec['reason_key'] = 'tamponade_skip_open_shell'
                rec['reason'] = ('not suitable: open shell / no solid interior; '
                                 'left to Flap')
            else:
                rec['reason_key'] = 'tamponade_skip_no_interior'
                rec['reason'] = ('not suitable: thin shell / no interior volume; '
                                 'left to Flap')
            stats['tamponade'] = rec
            return v, t, after
        if tamponade_force and shape['skip']:
            rec['gate'] = 'user_forced'

        seal_budget = None
        if prepass and prepass_budget is not None:
            seal_budget = int(prepass_budget)
        out_v, out_t, trec = _tamp.run_tamponade(
            cur_v, cur_t, intensity=intensity_val,
            work_budget=seal_budget)
        for key in ('work_budget', 'work_units', 'h_min', 'seconds',
                    'plugs', 'plug_faces',
                    'plugs_rejected', 'plugs_inconclusive',
                    'plugs_rejected_size', 'plugs_rejected_dev',
                    'melt_layers_tried', 'melt_layers_accepted',
                    'melt_layers_rolled_back', 'micro_welds_ok',
                    'micro_welds_failed', 'excised_faces',
                    'healthy_faces_kept',
                    'reference_used', 'reference_fallback',
                    'stent_used', 'stent_rejected', 'stent_subplugs',
                    'plug_metrics',
                    'trimmed_vertices', 'patch_vertices',
                    'plugs_rolled_back', 'plugs_leaking',
                    'plugs_double_wall', 'plugs_floating',
                    'components_before', 'components_after',
                    'closed_shells_before', 'closed_shells_after'):
            rec[key] = trec.get(key)
        rec['reason'] = trec.get('reason')
        if not trec.get('ran'):
            stats['tamponade'] = rec
            return v, t, after
        rec['ran'] = True

        cand_v = np.asarray(out_v, dtype=np.float32)
        cand_t = np.asarray(out_t, dtype=np.int32)
        _dump_tamp_mesh(dump_dir, 'tamp_output', cand_v, cand_t)
        rec['output_faces'] = int(len(cand_t))
        cand_holes, cand_nm = reload_strict_holes_nm(cand_v, cand_t)
        wv, wt = weld_reload_equivalent(cand_v, cand_t)
        cand_parts = _count_vertex_components(len(wv), wt)
        rec['candidate_holes'] = cand_holes
        rec['candidate_non_manifold'] = cand_nm
        rec['candidate_parts'] = cand_parts
        healthy = trec.get('healthy_faces_kept')
        if healthy is not None and len(cur_t):
            rec['healthy_fraction'] = float(healthy) / float(len(cur_t))

        # Internal walls are REPORT-ONLY under the leak rule: a buried face is
        # inside the solid and a slicer unions it fine, so it no longer blocks
        # adoption. The winding query costs ~100 s on a 400k-face mesh, so it is
        # computed only on request (SUTURA_TAMP_IW).
        plugs_added = int(rec.get('plugs') or 0)
        if plugs_added and os.environ.get('SUTURA_TAMP_IW'):
            cand_iw = _internal_wall_count(cand_v, cand_t)
            rec['candidate_internal_walls'] = cand_iw
            if cand_iw > 0:
                rec['input_internal_walls'] = _internal_wall_count(cur_v, cur_t)

        no_rise = cand_holes <= base_holes and cand_nm <= base_nm
        strict_fall = (cand_holes + cand_nm) < (base_holes + base_nm)
        parts_ok = cand_parts <= base_parts
        healthy_ok = (healthy is None or len(cur_t) == 0
                      or float(healthy) >= 0.99 * float(len(cur_t)))
        # Exact self-intersection fall (review C2): a melt-only improvement on a
        # mesh whose holes/non-manifold are already 0 must still be adoptable.
        # The prepass only compares the faces the candidate changed (cheap on a
        # large scan); post-chain keeps the full scan of both meshes.
        cand_si_exact = None
        base_local = cand_local = None
        if prepass:
            base_local, cand_local, si_rise, local_used = _local_si_rise(
                cur_v, cur_t, wv, wt)
            if local_used:
                rec['si_method'] = 'local'
                rec['base_local_si'] = base_local
                rec['cand_local_si'] = cand_local
                si_fall = bool(cand_local < base_local)
                si_ok = bool(not si_rise)
            elif base_holes == 0 and base_nm == 0:
                # Stale extension without the SI-subset binding and an already
                # closed base: there is no holes/non-manifold fall the prepass
                # could adopt on, so skip the two full exact scans. The prepass
                # is an optimization only, so not adopting here is always safe.
                rec['si_method'] = 'unavailable'
                si_fall = False
                si_ok = False
            else:
                base_si_exact = _exact_si_face_count(wb_v, wb_t)
                rec['input_self_intersections'] = base_si_exact
                cand_si_exact = (_exact_si_face_count(wv, wt)
                                 if base_si_exact is not None else None)
                rec['candidate_self_intersections'] = cand_si_exact
                si_fall = (base_si_exact is not None
                           and cand_si_exact is not None
                           and cand_si_exact < base_si_exact)
                si_ok = (base_si_exact is None or cand_si_exact is None
                         or cand_si_exact <= base_si_exact)
                rec['si_method'] = 'full'
        else:
            if base_si_exact is not None:
                cand_si_exact = _exact_si_face_count(wv, wt)
                rec['candidate_self_intersections'] = cand_si_exact
            si_fall = (base_si_exact is not None and cand_si_exact is not None
                       and cand_si_exact < base_si_exact)
            si_ok = (base_si_exact is None or cand_si_exact is None
                     or cand_si_exact <= base_si_exact)
        rec['si_fall'] = si_fall
        rec['si_ok'] = si_ok
        guard = check_candidate(
            len(cur_t), len(cur_v), len(cand_t), len(cand_v),
            slack_units=int(rec.get('plug_faces') or 0),
            base_si=(base_si_exact if not prepass else None),
            candidate_si=(cand_si_exact if not prepass else None),
            base_arrays=(wb_v, wb_t), candidate_arrays=(wv, wt),
            measure_si=(not prepass and cand_si_exact is None),
            policy=policy_for('tamponade'))
        for key, value in guard.fields.items():
            if key in rec:
                rec[key] = value
        guard_ok = guard.ok
        # Prepass placement: the Stage-1 chain still runs after the seal and
        # repairs the non-manifold edge the reference plug can leave behind
        # (and re-caps any small loop its repair opens), so the gate only
        # requires the seal to strictly reduce the HOLES and keep the healthy
        # faces. Post-chain placement keeps the stricter holes+non-manifold
        # gate because nothing repairs the candidate after it.
        if prepass:
            geometry_fall = (cand_holes < base_holes) or si_fall
            no_rise = cand_holes <= base_holes
            # The prepass must not raise the exact self-intersection count of the
            # mesh it seals. The reference plug can graze nearby crater geometry
            # and add self-intersections the downstream Stage-1 chain does not
            # fully reconcile (measured: ornate-frame 66584 -> 66749, 1038441
            # 2350 -> 2436), so `si_ok` is kept as the strict non-rise guard
            # computed above rather than forced True.
            if (prepass_budget is not None
                    and int(rec.get('work_units') or 0) > int(prepass_budget)):
                si_ok = False
                rec['reject_reason'] = 'prepass work budget exceeded'
                rec['budget_limited_by'] = 'work'
        else:
            geometry_fall = strict_fall or si_fall
        # Plug leak guard: any plug added by Tamponade whose added vertices
        # lie outside the input solid beyond the shape tolerance fails
        # admission. Evaluated via _tamponade_shape_leak (which tests both
        # classic and antipodal oracles under the conservative rule).
        cand_leak_pts = 0
        cand_leak_mm = 0.0
        if plugs_added > 0:
            cand_leak_mm, cand_leak_pts = _tamponade_shape_leak(
                cur_v, cur_t, cand_v, cand_t, _bbox_diagonal(cur_v))
        leak_ok = (cand_leak_pts == 0)

        # Post-chain placement: self-intersections are REPORT-ONLY (slicers
        # union overlapping volumes) and internal walls are report-only too, so
        # neither blocks adoption; the Rust leak gate and the reload-honest
        # holes/non-manifold guard above are the real safety. The prepass keeps
        # its strict SI non-rise guard (`si_guard`) because Stage 1 does not
        # fully reconcile the SI a reference plug can add.
        si_guard = si_ok if prepass else True
        if (guard_ok and no_rise and geometry_fall and parts_ok and healthy_ok and si_guard
                and leak_ok and len(cand_t)):
            after = topology.mesh_topology(cand_v, cand_t)
            rec['adopted'] = True
            wet = bool(cand_holes == 0 and cand_nm == 0)
            rec['watertight'] = wet
            rec['reload_watertight'] = wet
            if rec.get('si_method') == 'local':
                si_desc = '%s -> %s (local)' % (base_local, cand_local)
            else:
                si_desc = '%s -> %s' % (base_si_exact, cand_si_exact)
            rec['reason'] = ('adopted (holes %d -> %d, non-manifold %d -> %d, '
                             'SI %s)'
                             % (base_holes, cand_holes, base_nm, cand_nm,
                                si_desc))
        else:
            reasons = []
            if not no_rise:
                reasons.append('holes/non-manifold rose')
            if not geometry_fall:
                reasons.append('holes+non-manifold/SI did not strictly fall')
            if not si_guard:
                if rec.get('reject_reason') == 'prepass budget exceeded':
                    reasons.append('prepass budget exceeded')
                else:
                    reasons.append('self-intersections rose')
            if not parts_ok:
                reasons.append('component count rose')
            if not healthy_ok:
                reasons.append('healthy faces below 99%')
            if not leak_ok:
                reasons.append('plug leaks outside solid (%.4f mm, %d pts)' % (cand_leak_mm, cand_leak_pts))
            if not len(cand_t):
                reasons.append('no geometry')
            rec['reason'] = 'not adopted (%s)' % '; '.join(reasons or ['?'])
            rec['reject_reason'] = ', '.join(reasons or ['?'])
    if 'tamponade_shell_prepass' in stats:
        rec['shell_prepass'] = stats['tamponade_shell_prepass']
    stats['tamponade'] = rec
    if rec['adopted']:
        return (np.asarray(cand_v, dtype=np.float64),
                np.asarray(cand_t, dtype=np.int64), after)
    return v, t, after
