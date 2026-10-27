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
* ``healthy_fill`` - volume the output adds outside the input on healthy
  skin, and volume it removes.  A surface-normal lattice (area samples x
  offset layers) is used instead of a full 3-D band grid: the latter cannot
  resolve a 0.1 mm coat on a 100 mm part without 10^7+ cells.

The input defect report and face mask are supplied by the caller, so the
module does not depend on ``defects.py`` and cannot drift from the repair's
own classification.
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
AREA_SAMPLES = 10000
MAX_AREA_SAMPLES = 30000
OFFSET_LAYERS = 16
BAND_FACTOR = 8.0
BAND_FRACTION = 0.01
WINDING_CHUNK = 65536


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


def _sharp_edges(v, t, face_mask=None, angle_deg=SHARP_ANGLE_DEG):
    """(edges, dihedral_deg) for manifold edges above the angle threshold."""
    t = np.asarray(t, dtype=np.int64)
    v = np.asarray(v, dtype=np.float64)
    if len(t) == 0:
        return np.zeros((0, 2), np.int64), np.zeros(0)
    normals, valid_face = _face_normals(v, t)
    edges, counts, inv = edge_table(t)
    half_face = np.tile(np.arange(len(t)), 3)
    sel = counts[inv] == 2
    if not sel.any():
        return np.zeros((0, 2), np.int64), np.zeros(0)
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
        return np.zeros((0, 2), np.int64), np.zeros(0)
    # unique(eidx) also restores edge-table order; map dihedral back by edge
    by_edge = np.full(len(edges), np.nan)
    by_edge[eidx[keep]] = dih[keep]
    e = edges[chosen]
    return e, by_edge[chosen]


def _sample_sharp(v, edges, dih, step, max_per_edge):
    """Stratified midpoints per edge, length-weighted and order-free.

    ``n`` depends only on this edge's length and the fixed ``step``, never on
    the rest of the mesh; identical edges therefore get identical samples and
    an identity comparison scores exactly 1.0.
    """
    v = np.asarray(v, dtype=np.float64)
    if len(edges) == 0:
        return np.zeros((0, 3)), np.zeros(0), np.zeros(0)
    lengths = np.linalg.norm(v[edges[:, 1]] - v[edges[:, 0]], axis=1)
    keep = lengths > 0.0
    if not keep.any():
        return np.zeros((0, 3)), np.zeros(0), np.zeros(0)
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
                   strict_deg=STRICT_DIHEDRAL_DEG):
    """Fraction of healthy input sharp-edge length the output represents.

    A sample is kept when the nearest output sharp-edge sample is within
    ``tol`` AND the two dihedral angles differ by less than ``match_deg``.
    Identity gives 1.0 exactly.
    """
    vi = np.asarray(vi, dtype=np.float64)
    ti = np.asarray(ti, dtype=np.int64)
    vo = np.asarray(vo, dtype=np.float64)
    to = np.asarray(to, dtype=np.int64)
    tol = float(tol if tol is not None else default_tol(vi))
    in_e, in_d = _sharp_edges(vi, ti, face_mask=defect_face_mask,
                              angle_deg=angle_deg)
    out_e, out_d = _sharp_edges(vo, to, face_mask=None, angle_deg=angle_deg)
    q, qd, qw = _sample_sharp(vi, in_e, in_d, tol, max_per_edge)
    r, rd, _rw = _sample_sharp(vo, out_e, out_d, tol, max_per_edge)

    out = {
        'feature_tol_mm': round(tol, 6),
        'feature_angle_deg': float(angle_deg),
        'feature_match_deg': float(match_deg),
        'feature_input_sharp_edges': int(len(in_e)),
        'feature_output_sharp_edges': int(len(out_e)),
        'feature_samples': int(len(q)),
        'feature_total_length_mm': round(float(qw.sum()), 4),
        'feature_kept_length_mm': 0.0,
        'feature_recall': 1.0 if len(q) == 0 else 0.0,
        'feature_recall_tol_half': 1.0 if len(q) == 0 else 0.0,
        'feature_recall_10deg': 1.0 if len(q) == 0 else 0.0,
        'feature_dihedral_delta_p50_deg': 0.0,
        'feature_dihedral_delta_p95_deg': 0.0,
    }
    if len(q) == 0:
        return out
    if len(r) == 0:
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


def healthy_fill(vi, ti, vo, to, defect_report=None,
                 defect_face_mask=None, tol=None, k_tol=K_TOL,
                 area_samples=AREA_SAMPLES, max_area_samples=MAX_AREA_SAMPLES,
                 layers=OFFSET_LAYERS, band_factor=BAND_FACTOR,
                 band_fraction=BAND_FRACTION):
    """Added / removed volume on healthy skin.

    A surface-normal lattice is evaluated in a band around the input.  A
    base sample is healthy when its face is outside the defect mask and its
    position is outside every expanded defect influence box; the box rule is
    what keeps a legitimate hole cap out of the metric.  Identity gives 0.
    """
    t0 = time.perf_counter()
    vi = np.asarray(vi, dtype=np.float64)
    ti = np.asarray(ti, dtype=np.int64)
    vo = np.asarray(vo, dtype=np.float64)
    to = np.asarray(to, dtype=np.int64)
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
        'healthy_band_mm': round(band, 6),
        'healthy_delta_mm': round(delta, 6),
        'healthy_area_samples': int(area_samples),
        'healthy_offset_layers': int(layers),
        'healthy_samples': 0,
        'healthy_input_volume_mm3': None,
        'healthy_volume_basis': 'unavailable',
        'healthy_defect_influence_mm': 0.0,
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
    healthy = ~mask[face_idx]
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
            recall_samples_per_edge=RECALL_SAMPLES_PER_EDGE,
            area_samples=AREA_SAMPLES, layers=OFFSET_LAYERS):
    """Both metrics as one flat dict, ready to merge into a verify result."""
    mask = defect_face_mask(ti, defect_report)
    out = feature_recall(vi, ti, vo, to, defect_face_mask=mask, tol=tol,
                         max_per_edge=recall_samples_per_edge)
    out.update(healthy_fill(vi, ti, vo, to, defect_report=defect_report,
                            defect_face_mask=mask, tol=tol,
                            area_samples=area_samples, layers=layers))
    return out
