#!/usr/bin/env python3
"""Surface-damage metrics shared by the verify harness and the release gate.

Both measurements are MEASUREMENT tools: nothing in ``sutura_engine`` /
``repair.py`` imports this module.  They are independent of the tool's own
JSON, deterministic (no RNG, fixed work budgets) and cheap enough for a corpus
row.

* ``feature_recall`` - how much of the input's sharp-edge length the output
  still represents with a matching dihedral.  Input edges are "healthy"
  (both incident faces outside the input defect mask); the output is the
  candidate, so every sharp output edge is a match candidate.
  ``feature_recall_exterior`` is the primary value.  When the input has
  Case-B interior walls (an overlap of its own parts, classified on the input
  alone by ``sutura_geom.buried_face_mask``) it excludes their edges; the old
  output-inside-parity exterior is kept as the ``feature_exterior_output_*``
  diagnostic.  ``feature_recall`` remains the all-input secondary value.
* ``healthy_fill`` - volume the output adds outside the input on healthy
  skin, and volume it removes.  A surface-normal lattice (area samples x
  offset layers) is used instead of a full 3-D band grid: the latter cannot
  resolve a 0.1 mm coat on a 100 mm part without 10^7+ cells.
  Deep-buried input faces (distance to the output surface greater than the
  sampling band) are ignored for added/removed volume: they are internal
  shells, not printed skin.  A shallow coat stays, because its distance to
  the output surface is below the band.

The input defect report and face mask are supplied by the caller, so the
module does not depend on ``defects.py`` and cannot drift from the repair's
own classification.

Sampling budgets are named: ``sweep`` (4k area samples x 10 offset layers,
the default for corpus sweeps) and ``gate`` (10k x 16, for final gates).
Every result records ``healthy_budget`` plus the actual area/layer counts, and
every report must show it.  Fill/removed are normal-lattice prism estimates:
adjacent normal prisms overlap at convex edges, so the absolute mm3 is biased;
use them for relative before/after comparison, not absolute metrology.
"""
import time

import numpy as np

try:  # optional: the feature metric works without the extension
    import sutura_geom
except Exception:  # noqa: BLE001
    sutura_geom = None

from topology import edge_table

SHARP_ANGLE_DEG = 30.0
DIHEDRAL_MATCH_DEG = 15.0
STRICT_DIHEDRAL_DEG = 10.0
RECALL_SAMPLES_PER_EDGE = 8
DETAIL_TOL_MM = 0.1
DETAIL_TOL_FRACTION = 0.002
K_TOL = 4.0
SWEEP_AREA_SAMPLES = 4000
SWEEP_OFFSET_LAYERS = 10
GATE_AREA_SAMPLES = 10000
GATE_OFFSET_LAYERS = 16
SURFACE_BUDGETS = {
    'sweep': (SWEEP_AREA_SAMPLES, SWEEP_OFFSET_LAYERS),
    'gate': (GATE_AREA_SAMPLES, GATE_OFFSET_LAYERS),
}
DEFAULT_SURFACE_BUDGET = 'sweep'
# Backward-compatible aliases for the default (sweep) budget.
AREA_SAMPLES = SWEEP_AREA_SAMPLES
OFFSET_LAYERS = SWEEP_OFFSET_LAYERS
MAX_AREA_SAMPLES = 30000
BAND_FACTOR = 8.0
BAND_FRACTION = 0.01
WINDING_CHUNK = 65536


def resolve_budget(budget=None, area_samples=None, layers=None):
    """Resolve a named budget, returning (area, layers, label)."""
    name = str(budget or DEFAULT_SURFACE_BUDGET).lower()
    if name not in SURFACE_BUDGETS:
        raise ValueError('unknown surface budget %r (expected sweep or gate)'
                         % budget)
    base_area, base_layers = SURFACE_BUDGETS[name]
    area = base_area if area_samples is None else int(area_samples)
    lay = base_layers if layers is None else int(layers)
    label = name if (area == base_area and lay == base_layers) else 'custom'
    return area, lay, label


def _bbox_diag(v):
    v = np.asarray(v, dtype=np.float64)
    if len(v) == 0:
        return 0.0
    return float(np.linalg.norm(v.max(0) - v.min(0)))


def default_tol(v):
    """Graft's detail tolerance: max(0.1 mm, 0.2 % of the bbox diagonal)."""
    diag = _bbox_diag(v)
    return max(DETAIL_TOL_MM, DETAIL_TOL_FRACTION * diag)


def defect_face_mask(tris, report=None):
    """(M,) bool: faces adjacent to a boundary or non-manifold edge.

    This is the narrow defect rim: it does not grow a vertex ring, so damage
    *next to* a repair is still measured.  Using the edge table (rather than
    every face touching a ``verts_idx``) also keeps a coarse open mesh from
    masking almost every face.  ``report['non_manifold'][*]['faces_idx']`` is
    unioned in when present.
    """
    t = np.asarray(tris, dtype=np.int64)
    mask = np.zeros(len(t), dtype=bool)
    if len(t) == 0:
        return mask
    _edges, counts, inv = edge_table(t)
    half_face = np.tile(np.arange(len(t)), 3)
    mask[half_face[counts[inv] != 2]] = True
    if report is not None:
        for nm in (report.get('non_manifold') or []):
            faces = nm.get('faces_idx')
            if faces is not None:
                idx = np.asarray(faces, dtype=np.int64)
                idx = idx[(idx >= 0) & (idx < len(t))]
                mask[idx] = True
    return mask


def _face_normals(v, t):
    a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
    cr = np.cross(b - a, c - a)
    ln = np.linalg.norm(cr, axis=1)
    ok = ln > 0.0
    n = np.zeros_like(cr)
    n[ok] = cr[ok] / ln[ok, None]
    return n, ok


def input_buried_face_mask(vi, ti, defect_face_mask=None, delta=0.1):
    """Input-alone Case-B interior-wall mask, via ``sutura_geom.buried_face_mask``.

    Returns ``(mask, info)``.  The kernel already excludes zero-area faces and
    floating debris (Case C); the defect mask is applied here because it lives
    in the Python pipeline.  Cavity walls are not marked.  ``info`` carries the
    counts/areas plus availability so a report can say why the split is absent.
    """
    ti = np.asarray(ti, dtype=np.int64)
    mask = np.zeros(len(ti), dtype=bool)
    info = {
        'buried_input_available': False,
        'buried_input_faces': 0,
        'buried_input_area_mm2': 0.0,
        'buried_input_debris_faces': 0,
        'buried_input_degenerate_faces': 0,
        'buried_input_epsilon_mm': 0.0,
        'buried_input_note': None,
    }
    try:
        import sutura_geom
        if not hasattr(sutura_geom, 'buried_face_mask'):
            info['buried_input_note'] = 'sutura_geom lacks buried_face_mask'
            return mask, info
        m, d = sutura_geom.buried_face_mask(
            np.asarray(vi, dtype=np.float64), ti.astype(np.int32),
            float(delta))
        mask = np.asarray(m, dtype=bool)
        info.update({
            'buried_input_available': True,
            'buried_input_debris_faces': int(d.get('debris_count', 0)),
            'buried_input_degenerate_faces': int(d.get('degenerate_count', 0)),
            'buried_input_epsilon_mm': round(float(d.get('epsilon_mm', 0.0)), 6),
        })
    except Exception as e:  # noqa: BLE001
        info['buried_input_note'] = 'buried_face_mask failed: %s' % e
        return mask, info
    if defect_face_mask is not None:
        mask &= ~np.asarray(defect_face_mask, dtype=bool)
    tri = np.asarray(vi, dtype=np.float64)[ti]
    areas = 0.5 * np.linalg.norm(
        np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0]), axis=1)
    info['buried_input_faces'] = int(np.count_nonzero(mask))
    info['buried_input_area_mm2'] = round(float(areas[mask].sum()), 6)
    return mask, info


def _sharp_edges(v, t, face_mask=None, angle_deg=SHARP_ANGLE_DEG,
                 with_faces=False):
    """(edges, dihedral_deg[, face_pairs]) above the angle threshold."""
    t = np.asarray(t, dtype=np.int64)
    v = np.asarray(v, dtype=np.float64)
    empty = (np.zeros((0, 2), np.int64), np.zeros(0))
    if with_faces:
        empty = empty + (np.zeros((0, 2), np.int64),)
    if len(t) == 0:
        return empty
    normals, valid_face = _face_normals(v, t)
    edges, counts, inv = edge_table(t)
    half_face = np.tile(np.arange(len(t)), 3)
    sel = counts[inv] == 2
    if not sel.any():
        return empty
    eids = inv[sel]
    faces = half_face[sel]
    order = np.argsort(eids, kind='stable')
    eids = eids[order]
    faces = faces[order]
    starts = np.flatnonzero(np.r_[True, eids[1:] != eids[:-1]])
    f0 = faces[starts]
    f1 = faces[starts + 1]
    eidx = eids[starts]
    ok = valid_face[f0] & valid_face[f1]
    if face_mask is not None:
        ok &= ~face_mask[f0] & ~face_mask[f1]
    dot = np.clip(np.einsum('ij,ij->i', normals[f0], normals[f1]), -1.0, 1.0)
    dih = np.degrees(np.arccos(dot))
    keep = ok & (dih > float(angle_deg))
    chosen = np.unique(eidx[keep])
    if len(chosen) == 0:
        return empty
    # unique(eidx) also restores edge-table order; map dihedral back by edge
    by_edge = np.full(len(edges), np.nan)
    by_edge[eidx[keep]] = dih[keep]
    e = edges[chosen]
    if with_faces:
        by_f0 = np.full(len(edges), -1, dtype=np.int64)
        by_f1 = np.full(len(edges), -1, dtype=np.int64)
        by_f0[eidx[keep]] = f0[keep]
        by_f1[eidx[keep]] = f1[keep]
        faces = np.stack([by_f0[chosen], by_f1[chosen]], axis=1)
        return e, by_edge[chosen], faces
    return e, by_edge[chosen]


def _sample_sharp(v, edges, dih, step, max_per_edge, edge_flag=None,
                  return_flag=False):
    """Stratified midpoints per edge, length-weighted and order-free.

    ``n`` depends only on this edge's length and the fixed ``step``, never on
    the rest of the mesh; identical edges therefore get identical samples and
    an identity comparison scores exactly 1.0.
    """
    v = np.asarray(v, dtype=np.float64)
    if len(edges) == 0:
        if return_flag:
            return np.zeros((0, 3)), np.zeros(0), np.zeros(0), np.zeros(0, bool)
        return np.zeros((0, 3)), np.zeros(0), np.zeros(0)
    lengths = np.linalg.norm(v[edges[:, 1]] - v[edges[:, 0]], axis=1)
    keep = lengths > 0.0
    if not keep.any():
        if return_flag:
            return np.zeros((0, 3)), np.zeros(0), np.zeros(0), np.zeros(0, bool)
        return np.zeros((0, 3)), np.zeros(0), np.zeros(0)
    if edge_flag is not None:
        edge_flag = np.asarray(edge_flag, dtype=bool)[keep]
    edges = edges[keep]
    dih = dih[keep]
    lengths = lengths[keep]
    step = max(float(step), 1e-12)
    n = np.minimum(
        np.maximum(1, np.floor(lengths / step + 0.5).astype(np.int64)),
        int(max_per_edge))
    idx = np.repeat(np.arange(len(edges)), n)
    starts = np.cumsum(n) - n
    k = np.arange(len(idx)) - np.repeat(starts, n)
    frac = (k + 0.5) / n[idx]
    p = (v[edges[idx, 0]] * (1.0 - frac)[:, None]
         + v[edges[idx, 1]] * frac[:, None])
    w = lengths[idx] / n[idx]
    if return_flag:
        flag = edge_flag[idx] if edge_flag is not None else None
        return p, dih[idx], w, flag
    return p, dih[idx], w


def _nearest_within(query, ref, tol):
    """Nearest reference point per query via a uniform hash (no scipy)."""
    nq, nr = len(query), len(ref)
    best = np.full(nq, np.inf)
    best_i = np.full(nq, -1, dtype=np.int64)
    if nq == 0 or nr == 0 or tol <= 0.0:
        return best, best_i
    cell_r = np.floor(ref / tol).astype(np.int64)
    cell_q = np.floor(query / tol).astype(np.int64)
    shift = np.minimum(cell_r.min(0), cell_q.min(0))
    cr = cell_r - shift
    cq = cell_q - shift
    k = int(max(cr.max(), cq.max())) + 2
    if float(k) ** 3 >= 2.0 ** 62:  # pathological size/tolerance ratio
        return best, best_i

    def key(c):
        return (c[:, 0] * k + c[:, 1]) * k + c[:, 2]

    kr = key(cr)
    order = np.argsort(kr, kind='stable')
    sk = kr[order]
    for dx in (-1, 0, 1):
        for dy in (-1, 0, 1):
            for dz in (-1, 0, 1):
                qk = key(cq + np.array([dx, dy, dz], dtype=np.int64))
                left = np.searchsorted(sk, qk, side='left')
                right = np.searchsorted(sk, qk, side='right')
                span = right - left
                if not span.any():
                    continue
                # A grid cell can hold several reference samples; consider
                # every one of them, not just the first.
                for j in range(int(span.max())):
                    valid = (left + j) < right
                    qi = np.nonzero(valid)[0]
                    if len(qi) == 0:
                        continue
                    cand = order[left[qi] + j]
                    d = np.linalg.norm(query[qi] - ref[cand], axis=1)
                    better = d < best[qi]
                    if better.any():
                        qi2 = qi[better]
                        best[qi2] = d[better]
                        best_i[qi2] = cand[better]
    return best, best_i


def feature_recall(vi, ti, vo, to, defect_face_mask=None, tol=None,
                   max_per_edge=RECALL_SAMPLES_PER_EDGE,
                   angle_deg=SHARP_ANGLE_DEG,
                   match_deg=DIHEDRAL_MATCH_DEG,
                   strict_deg=STRICT_DIHEDRAL_DEG, exterior=None,
                   buried_mask=None):
    """Fraction of healthy input sharp-edge length the output represents.

    A sample is kept when the nearest output sharp-edge sample is within
    ``tol`` AND the two dihedral angles differ by less than ``match_deg``.
    Identity gives 1.0 exactly.

    When ``buried_mask`` (input-alone Case-B interior walls) marks any face,
    the primary exterior set becomes the visible input edges; the output-based
    exterior (vertex inside the output solid) is kept as a diagnostic.  With no
    buried faces the old output-based behaviour is unchanged.
    """
    vi = np.asarray(vi, dtype=np.float64)
    ti = np.asarray(ti, dtype=np.int64)
    vo = np.asarray(vo, dtype=np.float64)
    to = np.asarray(to, dtype=np.int64)
    tol = float(tol if tol is not None else default_tol(vi))
    if exterior is None:
        exterior = _inside_output(vo, to, vi)
    inside_v, out_dist, ext_avail, ext_note = exterior
    in_e, in_d, in_faces = _sharp_edges(vi, ti, face_mask=defect_face_mask,
                                        angle_deg=angle_deg, with_faces=True)
    out_e, out_d = _sharp_edges(vo, to, face_mask=None, angle_deg=angle_deg)
    ext_mask_output = None
    if ext_avail and len(in_e):
        f0 = in_faces[:, 0]
        f1 = in_faces[:, 1]
        ext_mask_output = ~((inside_v[ti[f0]].any(axis=1))
                            | (inside_v[ti[f1]].any(axis=1)))
    ext_mask_input = None
    buried_active = False
    if buried_mask is not None:
        bm = np.asarray(buried_mask, dtype=bool)
        if len(bm) == len(ti):
            buried_active = bool(bm.any())
            if len(in_e):
                f0 = in_faces[:, 0]
                f1 = in_faces[:, 1]
                ext_mask_input = ~(bm[f0] | bm[f1])
    use_input = buried_active and ext_mask_input is not None
    ext_mask = ext_mask_input if use_input else ext_mask_output
    ext_available = bool(ext_avail or use_input)
    ext_note_final = ext_note
    if use_input:
        ext_note_final = ('input-alone buried mask'
                          if ext_avail else
                          'input-alone buried mask; output exterior unavailable')
    source = ('input_buried_mask' if use_input else
              ('output' if ext_mask_output is not None else 'none'))
    q, qd, qw, ext_sample = _sample_sharp(
        vi, in_e, in_d, tol, max_per_edge, edge_flag=ext_mask,
        return_flag=True)
    r, rd, _rw = _sample_sharp(vo, out_e, out_d, tol, max_per_edge)

    out = {
        'feature_tol_mm': round(tol, 6),
        'feature_angle_deg': float(angle_deg),
        'feature_match_deg': float(match_deg),
        'feature_input_sharp_edges': int(len(in_e)),
        'feature_output_sharp_edges': int(len(out_e)),
        'feature_samples_per_edge': int(max_per_edge),
        'feature_samples': int(len(q)),
        'feature_total_length_mm': round(float(qw.sum()), 4),
        'feature_kept_length_mm': 0.0,
        'feature_recall': 1.0 if len(q) == 0 else 0.0,
        'feature_recall_tol_half': 1.0 if len(q) == 0 else 0.0,
        'feature_recall_10deg': 1.0 if len(q) == 0 else 0.0,
        'feature_dihedral_delta_p50_deg': 0.0,
        'feature_dihedral_delta_p95_deg': 0.0,
        'feature_exterior_available': ext_available,
        'feature_exterior_note': ext_note_final,
        'feature_exterior_source': source,
        'feature_exterior_sharp_edges': (
            int(ext_mask.sum()) if ext_mask is not None else None),
        'feature_buried_sharp_edges': (
            int((~ext_mask).sum()) if ext_mask is not None else None),
        'feature_exterior_total_length_mm': 0.0,
        'feature_exterior_kept_length_mm': 0.0,
        'feature_buried_length_mm': 0.0,
        'feature_buried_fraction': 0.0,
        'feature_recall_exterior': (
            1.0 if (ext_available and (ext_mask is None or not ext_mask.any()))
            else (None if not ext_available else 0.0)),
        'feature_recall_exterior_tol_half': (
            1.0 if (ext_available and (ext_mask is None or not ext_mask.any()))
            else (None if not ext_available else 0.0)),
        'feature_recall_exterior_10deg': (
            1.0 if (ext_available and (ext_mask is None or not ext_mask.any()))
            else (None if not ext_available else 0.0)),
        'feature_exterior_dihedral_delta_p50_deg': 0.0,
        'feature_exterior_dihedral_delta_p95_deg': 0.0,
        'feature_exterior_output_available': bool(ext_avail),
        'feature_exterior_output_sharp_edges': (
            int(ext_mask_output.sum()) if ext_mask_output is not None else None),
        'feature_exterior_output_buried_sharp_edges': (
            int((~ext_mask_output).sum()) if ext_mask_output is not None else None),
        'feature_exterior_output_recall': None,
    }
    if len(q) == 0:
        return out
    if len(r) == 0:
        if ext_sample is not None:
            out['feature_recall_exterior'] = (
                0.0 if ext_sample.any() else 1.0)
            out['feature_recall_exterior_tol_half'] = out['feature_recall_exterior']
            out['feature_recall_exterior_10deg'] = out['feature_recall_exterior']
        return out

    dist, idx = _nearest_within(q, r, tol)
    dist_ok = (idx >= 0) & (dist <= tol)
    delta = np.full(len(q), np.inf)
    delta[dist_ok] = np.abs(qd[dist_ok] - rd[idx[dist_ok]])
    keep = dist_ok & (delta < float(match_deg))
    keep_half = (idx >= 0) & (dist <= 0.5 * tol) & (delta < float(match_deg))
    keep_strict = dist_ok & (delta < float(strict_deg))
    total = float(qw.sum()) or 1.0
    out['feature_kept_length_mm'] = round(float(qw[keep].sum()), 4)
    out['feature_recall'] = float(qw[keep].sum() / total)
    out['feature_recall_tol_half'] = float(qw[keep_half].sum() / total)
    out['feature_recall_10deg'] = float(qw[keep_strict].sum() / total)
    matched = delta[dist_ok]
    if len(matched):
        out['feature_dihedral_delta_p50_deg'] = round(
            float(np.percentile(matched, 50)), 3)
        out['feature_dihedral_delta_p95_deg'] = round(
            float(np.percentile(matched, 95)), 3)
    if ext_sample is not None:
        ext_total = float(qw[ext_sample].sum())
        out['feature_exterior_total_length_mm'] = round(ext_total, 4)
        out['feature_exterior_kept_length_mm'] = round(
            float(qw[keep & ext_sample].sum()), 4)
        out['feature_buried_length_mm'] = round(
            float(qw[~ext_sample].sum()), 4)
        out['feature_buried_fraction'] = float(
            qw[~ext_sample].sum() / total)
        if ext_total > 0.0:
            out['feature_recall_exterior'] = float(
                qw[keep & ext_sample].sum() / ext_total)
            out['feature_recall_exterior_tol_half'] = float(
                qw[keep_half & ext_sample].sum() / ext_total)
            out['feature_recall_exterior_10deg'] = float(
                qw[keep_strict & ext_sample].sum() / ext_total)
            matched_ext = delta[dist_ok & ext_sample]
            if len(matched_ext):
                out['feature_exterior_dihedral_delta_p50_deg'] = round(
                    float(np.percentile(matched_ext, 50)), 3)
                out['feature_exterior_dihedral_delta_p95_deg'] = round(
                    float(np.percentile(matched_ext, 95)), 3)
    if use_input and ext_mask_output is not None:
        _qo, _qdo, _qwo, ext_sample_out = _sample_sharp(
            vi, in_e, in_d, tol, max_per_edge, edge_flag=ext_mask_output,
            return_flag=True)
        if ext_sample_out is not None and len(ext_sample_out) == len(qw):
            out_total = float(qw[ext_sample_out].sum())
            if out_total > 0.0:
                out['feature_exterior_output_recall'] = float(
                    qw[keep & ext_sample_out].sum() / out_total)
            else:
                out['feature_exterior_output_recall'] = (
                    1.0 if not ext_sample_out.any() else 0.0)
    return out


def _defect_influence(vi, report, tol, diag, k_tol=K_TOL):
    """Expanded defect bboxes, plus rim points for model-spanning open loops.

    A hole cap is hidden by the expanded bbox of its rim.  A boundary loop
    that spans the model (an open sheet) must not hide the whole sheet, so
    such a defect contributes a narrow band around its rim instead.
    """
    boxes = []
    rim_pts = []
    halo = 0.0
    rim_halo = 0.0
    if report is None:
        return boxes, rim_pts, halo, rim_halo
    groups = (report.get('holes') or []) + (report.get('non_manifold') or [])
    for d in groups:
        idx = d.get('verts_idx')
        if not idx:
            continue
        pts = np.asarray(vi, dtype=np.float64)[np.asarray(idx, dtype=np.int64)]
        if len(pts) == 0:
            continue
        lo, hi = pts.min(0), pts.max(0)
        centre = pts.mean(0)
        extent = float(np.linalg.norm(pts - centre, axis=1).max())
        if diag > 0.0 and extent >= 0.45 * diag:
            rim_pts.append(pts)
            rim_halo = max(rim_halo, max(float(k_tol) * tol, 0.01 * diag))
            continue
        h = max(float(k_tol) * tol, 0.5 * extent)
        boxes.append((lo - h, hi + h))
        halo = max(halo, h)
    if rim_pts:
        rim_pts = np.vstack(rim_pts)
    else:
        rim_pts = np.zeros((0, 3))
    return boxes, rim_pts, halo, rim_halo


def _in_boxes(points, boxes):
    inside = np.zeros(len(points), dtype=bool)
    for lo, hi in boxes:
        inside |= np.all((points >= lo) & (points <= hi), axis=1)
    return inside


def _area_samples(v, t, n_total, max_total):
    """Deterministic area-weighted barycentric samples (one weight each).

    Samples are placed by cumulative face area, so the budget is independent
    of the face count and a dense mesh cannot starve small faces.  The weight
    is ``total_area / n`` for every sample.
    """
    a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
    cr = np.cross(b - a, c - a)
    area = 0.5 * np.linalg.norm(cr, axis=1)
    total = float(area.sum())
    n = int(min(int(n_total), int(max_total)))
    if total <= 0.0 or n <= 0 or len(t) == 0:
        return (np.zeros((0, 3)), np.zeros((0, 3)), np.zeros(0),
                np.zeros(0, np.int64))
    u_area = (np.arange(n, dtype=np.float64) + 0.5) / n * total
    cdf = np.cumsum(area)
    idx = np.searchsorted(cdf, u_area, side='right')
    idx = np.clip(idx, 0, len(t) - 1)
    k = np.arange(n)
    u = np.mod((k + 0.5) * 0.7548776662466927, 1.0)
    w = np.mod((k + 0.5) * 0.5698402909980532, 1.0)
    flip = (u + w) > 1.0
    u[flip] = 1.0 - u[flip]
    w[flip] = 1.0 - w[flip]
    p = (a[idx] * (1.0 - u - w)[:, None] + b[idx] * u[:, None]
         + c[idx] * w[:, None])
    normals, _ok = _face_normals(v, t)
    keep = area[idx] > 0.0
    p = p[keep]
    idx = idx[keep]
    weight = np.full(len(p), total / n, dtype=np.float64)
    return p, normals[idx], weight, idx


def _winding(bvh, points):
    w = np.empty(len(points), dtype=np.float64)
    for s in range(0, len(points), WINDING_CHUNK):
        w[s:s + WINDING_CHUNK] = np.asarray(
            bvh.winding_points(points[s:s + WINDING_CHUNK]), dtype=np.float64)
    return w


def _signed_volume(v, t):
    a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
    return abs(float(np.einsum('ij,ij->i', a, np.cross(b, c)).sum() / 6.0))


def _gwn_volume(v, t, samples=12000, seed=0):
    """Deterministic Monte-Carlo GWN union volume (open-input fallback)."""
    if sutura_geom is None or len(v) == 0 or len(t) == 0:
        return None
    lo, hi = v.min(0), v.max(0)
    bbox_vol = float(np.prod(np.maximum(hi - lo, 1e-12)))
    rng = np.random.default_rng(int(seed))
    pts = lo + (hi - lo) * rng.random((int(samples), 3))
    bvh = sutura_geom.PyMeshBvh(v, np.asarray(t, dtype=np.int32))
    inside = _winding(bvh, pts) > 0.5
    return float(inside.mean() * bbox_vol)


def _is_closed(t):
    if len(t) == 0:
        return False
    _edges, counts, _inv = edge_table(t)
    return bool(len(counts) and np.all(counts == 2))


def _inside_output(vo, to, vi, n_dirs=3):
    """Per input vertex: buried inside the output solid?

    3-ray parity vote (``raystab_points``), plus a small surface-distance
    margin so vertices on the shared exterior are not classified as buried.
    Returns ``(inside, output_distance, available, note)``.
    """
    n = len(vi)
    inside = np.zeros(n, dtype=bool)
    dist = np.zeros(n, dtype=np.float64)
    if sutura_geom is None or n == 0:
        return inside, dist, False, 'sutura_geom unavailable'
    if len(to) == 0:
        return inside, dist, False, 'empty output'
    if not _is_closed(to):
        return inside, dist, False, 'output not watertight'
    try:
        res = sutura_geom.raystab_points(vo, to, vi, n_dirs=3, parity=True)
        votes = np.asarray(res[1], dtype=np.int64)
    except Exception as e:  # noqa: BLE001
        return inside, dist, False, 'raystab failed: %s' % e
    try:
        _closest, dd, _faces = sutura_geom.closest_points(vo, to, vi)
        dist = np.asarray(dd, dtype=np.float64)
    except Exception as e:  # noqa: BLE001
        return inside, dist, False, 'closest_points failed: %s' % e
    margin = max(1e-6, 1e-4 * _bbox_diag(vo))
    inside = (votes >= 2) & (dist > margin)
    return inside, dist, True, None


def healthy_fill(vi, ti, vo, to, defect_report=None,
                 defect_face_mask=None, tol=None, k_tol=K_TOL,
                 budget=DEFAULT_SURFACE_BUDGET,
                 area_samples=None, max_area_samples=MAX_AREA_SAMPLES,
                 layers=None, band_factor=BAND_FACTOR,
                 band_fraction=BAND_FRACTION, exterior=None, buried_mask=None):
    """Added / removed volume on healthy skin.

    A surface-normal lattice is evaluated in a band around the input.  A
    base sample is healthy when its face is outside the defect mask and its
    position is outside every expanded defect influence box; the box rule is
    what keeps a legitimate hole cap out of the metric.  Identity gives 0.

    When ``buried_mask`` (input-alone Case-B interior walls) is supplied, walls
    the output removed are also ignored: a correct union removes them, so they
    are not lost printed skin.  Debris and cavity walls are never in the mask.
    """
    t0 = time.perf_counter()
    vi = np.asarray(vi, dtype=np.float64)
    ti = np.asarray(ti, dtype=np.int64)
    vo = np.asarray(vo, dtype=np.float64)
    to = np.asarray(to, dtype=np.int64)
    if exterior is None:
        exterior = _inside_output(vo, to, vi)
    inside_v, out_dist_v, ext_avail, ext_note = exterior
    area_samples, layers, budget_label = resolve_budget(
        budget, area_samples, layers)
    tol = float(tol if tol is not None else default_tol(vi))
    diag = _bbox_diag(vi)
    band = max(float(band_factor) * tol, float(band_fraction) * diag)
    layers = int(layers)
    if layers % 2:
        layers += 1
    layers = max(2, layers)
    delta = 2.0 * band / layers
    out = {
        'healthy_fill_mm3': None,
        'healthy_fill_fraction': None,
        'healthy_removed_mm3': None,
        'healthy_removed_fraction': None,
        'healthy_budget': budget_label,
        'healthy_band_mm': round(band, 6),
        'healthy_delta_mm': round(delta, 6),
        'healthy_area_samples': int(area_samples),
        'healthy_offset_layers': int(layers),
        'healthy_samples': 0,
        'healthy_input_volume_mm3': None,
        'healthy_volume_basis': 'unavailable',
        'healthy_defect_influence_mm': 0.0,
        'healthy_exterior_available': bool(ext_avail),
        'healthy_exterior_note': ext_note,
        'healthy_buried_face_samples': 0,
        'healthy_buried_face_fraction': 0.0,
        'healthy_ignored_buried_face_samples': 0,
        'healthy_buried_input_face_samples': 0,
        'healthy_buried_input_face_fraction': 0.0,
        'healthy_ignored_input_wall_face_samples': 0,
        'healthy_input_wall_faces': 0,
        'healthy_input_wall_area_mm2': 0.0,
        'healthy_elapsed_ms': 0.0,
        'surface_metrics_note': None,
    }
    if sutura_geom is None:
        out['surface_metrics_note'] = 'sutura_geom unavailable'
        return out
    if len(vi) == 0 or len(ti) == 0:
        out['surface_metrics_note'] = 'empty input'
        return out

    mask = (np.asarray(defect_face_mask, dtype=bool)
            if defect_face_mask is not None
            else np.zeros(len(ti), dtype=bool))
    boxes, rim_pts, halo, rim_halo = _defect_influence(
        vi, defect_report, tol, diag, k_tol)
    out['healthy_defect_influence_mm'] = round(
        float(max(halo, rim_halo)), 6)

    base, normals, weight, face_idx = _area_samples(
        vi, ti, area_samples, max_area_samples)
    if len(base) == 0:
        out['surface_metrics_note'] = 'no area samples'
        return out
    deep_buried_face = np.zeros(len(ti), dtype=bool)
    buried_face = np.zeros(len(ti), dtype=bool)
    input_wall = np.zeros(len(ti), dtype=bool)
    input_wall_removed = np.zeros(len(ti), dtype=bool)
    if buried_mask is not None:
        bm = np.asarray(buried_mask, dtype=bool)
        if len(bm) == len(ti):
            input_wall = bm
    if ext_avail:
        inside_face = np.asarray(inside_v, dtype=bool)[ti]
        buried_face = np.all(inside_face, axis=1)
        face_out_dist = np.asarray(out_dist_v, dtype=np.float64)[ti].min(axis=1)
        deep_buried_face = buried_face & (face_out_dist > band)
        input_wall_removed = input_wall & (face_out_dist > band)
        deep_buried_face |= input_wall_removed
    buried = buried_face[face_idx]
    deep = deep_buried_face[face_idx]
    wall_samples = input_wall[face_idx]
    wall_removed_samples = input_wall_removed[face_idx]
    out['healthy_buried_face_samples'] = int(buried.sum())
    out['healthy_buried_face_fraction'] = float(buried.mean())
    out['healthy_ignored_buried_face_samples'] = int(deep.sum())
    out['healthy_buried_input_face_samples'] = int(wall_samples.sum())
    out['healthy_buried_input_face_fraction'] = float(wall_samples.mean())
    out['healthy_ignored_input_wall_face_samples'] = int(
        wall_removed_samples.sum())
    out['healthy_input_wall_faces'] = int(np.count_nonzero(input_wall))
    if input_wall.any():
        tri = vi[ti]
        wall_area = 0.5 * np.linalg.norm(
            np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0]), axis=1)
        out['healthy_input_wall_area_mm2'] = round(
            float(wall_area[input_wall].sum()), 6)
    healthy = ~mask[face_idx] & ~deep
    if boxes:
        healthy &= ~_in_boxes(base, boxes)
    if len(rim_pts):
        _d, _i = _nearest_within(base, rim_pts, rim_halo)
        healthy &= _d > rim_halo
    if not healthy.any():
        out['surface_metrics_note'] = 'no healthy samples'
        return out

    base = base[healthy]
    normals = normals[healthy]
    weight = weight[healthy]
    offsets = ((np.arange(layers, dtype=np.float64) + 0.5
                - layers / 2.0) * delta)
    pts = (base[:, None, :]
           + normals[:, None, :] * offsets[None, :, None]).reshape(-1, 3)
    vol_w = np.repeat(weight * delta, layers)
    if len(pts) == 0:
        out['surface_metrics_note'] = 'no lattice points'
        return out
    if boxes:
        vol_w = vol_w.copy()
        vol_w[_in_boxes(pts, boxes)] = 0.0
    if len(rim_pts):
        _rd, _ri = _nearest_within(pts, rim_pts, rim_halo)
        vol_w = vol_w.copy()
        vol_w[_rd <= rim_halo] = 0.0

    bvh_in = sutura_geom.PyMeshBvh(vi, np.asarray(ti, dtype=np.int32))
    bvh_out = sutura_geom.PyMeshBvh(vo, np.asarray(to, dtype=np.int32))
    same = (vi.shape == vo.shape and ti.shape == to.shape
            and np.array_equal(vi, vo) and np.array_equal(ti, to))
    w_in = _winding(bvh_in, pts)
    w_out = w_in if same else _winding(bvh_out, pts)
    added = (w_out > 0.5) & (w_in < 0.5)
    removed = (w_in > 0.5) & (w_out < 0.5)
    fill = float(vol_w[added].sum())
    rem = float(vol_w[removed].sum())

    if _is_closed(ti):
        vol_in = _signed_volume(vi, ti)
        basis = 'signed'
    else:
        vol_in = _gwn_volume(vi, ti)
        basis = 'gwn_approx'
    out.update({
        'healthy_fill_mm3': round(fill, 6),
        'healthy_removed_mm3': round(rem, 6),
        'healthy_samples': int(len(pts)),
        'healthy_input_volume_mm3': (round(float(vol_in), 6)
                                     if vol_in is not None else None),
        'healthy_volume_basis': basis,
        'healthy_elapsed_ms': round(1000.0 * (time.perf_counter() - t0), 1),
    })
    if vol_in and vol_in > 0.0:
        out['healthy_fill_fraction'] = float(fill / vol_in)
        out['healthy_removed_fraction'] = float(rem / vol_in)
    return out


def measure(vi, ti, vo, to, defect_report=None, *, tol=None,
            budget=DEFAULT_SURFACE_BUDGET,
            recall_samples_per_edge=RECALL_SAMPLES_PER_EDGE,
            area_samples=None, layers=None, buried=None):
    """Both metrics as one flat dict, ready to merge into a verify result.

    ``buried`` optionally supplies an already-computed
    ``(mask, info)`` interior-wall pair from the caller (same mesh, same delta,
    same defect mask).  ``buried_face_mask`` is a per-face winding pass over the
    whole surface, so on a large mesh it dominates the measurement; a caller
    that already ran it must not pay for it twice.
    """
    mask = defect_face_mask(ti, defect_report)
    if buried is not None:
        buried_mask, buried_info = buried
    else:
        buried_mask, buried_info = input_buried_face_mask(
            vi, ti, defect_face_mask=mask)
    exterior = _inside_output(np.asarray(vo), np.asarray(to),
                              np.asarray(vi))
    out = feature_recall(vi, ti, vo, to, defect_face_mask=mask, tol=tol,
                         max_per_edge=recall_samples_per_edge,
                         exterior=exterior, buried_mask=buried_mask)
    out.update(healthy_fill(vi, ti, vo, to, defect_report=defect_report,
                            defect_face_mask=mask, tol=tol, budget=budget,
                            area_samples=area_samples, layers=layers,
                            exterior=exterior, buried_mask=buried_mask))
    out.update(buried_info)
    out['surface_exterior_available'] = bool(exterior[2])
    out['surface_exterior_note'] = exterior[3]
    out['surface_exterior_buried_vertices'] = int(
        np.asarray(exterior[0], dtype=bool).sum())
    out['surface_budget'] = out.get('healthy_budget', 'custom')
    return out
