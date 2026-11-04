"""Repair Fidelity: "is it still the same model?"

A third, additive 0-100 axis next to Repair Health (topology) and Repair Risk
(how much the process moved). Fidelity compares the *shape* of the saved
result with the input surface on the healthy (non-defect) skin, and reports a
plain-language verdict.

Design rules:
  * Reporting only. Nothing here may choose, reject or reorder a repair
    candidate; the function is called on the final report after the repair has
    already been decided.
  * Fail-silent: any exception or unusable measurement yields
    ``repair_fidelity = None`` (status ``unavailable``); a repair never fails
    because of this module.
  * Deterministic: fixed sampling sequences, no RNG, no wall-clock decisions.
    The only size-dependent branch is the sample/face budget, which is a
    function of the mesh itself.
  * Cheap: one ``closest_points`` query plus bounded sharp-edge and
    normal-lattice work, all through the Rust ``sutura_geom`` kernels.
  * Weights, scales, tiers and budgets live in repair_score_config.json (the
    ``fidelity`` block); the built-in defaults mirror the shipped file.

Inputs (all from the final input/output arrays and the existing report):
  * healthy distance: one-sided output->input distance on healthy input skin,
  * feature-edge recall: healthy input sharp-edge length still represented,
  * healthy fill/removal volume,
  * volume + bounding-box change (sanity check; volume is also in Risk),
  * invented/new surface area (caps, rebuilds).

Large invented regions are surfaced as a "low confidence" flag: the explicit
tier evidence (fTetWild/Splint) or a large invented-area fraction means the
reconstruction there is a guess, not a measurement.
"""
import numpy as np

try:  # optional: the metric is unavailable without the shipped extension
    import sutura_geom
except Exception:  # noqa: BLE001
    sutura_geom = None

try:
    from topology import edge_table
except Exception:  # noqa: BLE001 - the pure-numpy oracle lives at top level
    edge_table = None

SHARP_ANGLE_DEG = 30.0
DIHEDRAL_MATCH_DEG = 15.0
DETAIL_TOL_MM = 0.1
DETAIL_TOL_FRACTION = 0.002

_DEFAULTS = {
    'fidelity': {
        'weights': {
            'healthy_distance': 35,
            'feature_recall': 25,
            'healthy_volume': 20,
            'volume_bbox': 10,
            'invented_area': 10,
        },
        'distance_scale_pct': 0.5,       # max healthy distance -> penalty 1
        'distance_mean_weight': 0.3,
        'volume_scale_fraction': 0.01,   # (fill + removed) / input volume
        'volume_change_scale_pct': 15.0,
        'bbox_scale_pct': 5.0,
        'invented_scale_fraction': 0.05,
        'change_tol_factor': 3.0,
        'tiers': {'high': 85, 'mid': 60},
        'low_confidence': {
            'invented_area_fraction': 0.03,
            'area_min_fraction': 0.0005,
            'low_area_min_fraction': 0.03,
            'max_areas': 99,
        },
        'budgets': {
            'full_faces': 20000,
            'max_faces': 150000,
            'distance_samples': 4000,
            'reduced_distance_samples': 1200,
            'sharp_samples_per_edge': 4,
            'reduced_sharp_samples_per_edge': 2,
            'recall_max_faces': 50000,
            'max_sharp_samples': 6000,
            'full_lattice_area': 1200,
            'full_lattice_layers': 6,
            'reduced_lattice_area': 400,
            'reduced_lattice_layers': 4,
        },
    },
}


def _deep_merge(base, override):
    out = dict(base)
    for k, v in (override or {}).items():
        if isinstance(v, dict) and isinstance(base.get(k), dict):
            out[k] = _deep_merge(base[k], v)
        else:
            out[k] = v
    return out


def load_config(path=None):
    """Load the fidelity config from repair_score_config.json (fail-silent)."""
    if isinstance(path, dict):
        return _deep_merge(_DEFAULTS, path)
    try:
        import repair_score
        cfg = repair_score.load_config(path)
    except Exception:  # noqa: BLE001
        cfg = {}
    return _deep_merge(_DEFAULTS, cfg)


def _as_f64(v):
    return np.asarray(v, dtype=np.float64)


def _as_i32(t):
    return np.asarray(t, dtype=np.int32).reshape(-1, 3)


def _bbox_diag(v):
    v = _as_f64(v)
    if v.size == 0:
        return 0.0
    return float(np.linalg.norm(v.max(0) - v.min(0)))


def _detail_tol(v):
    return max(DETAIL_TOL_MM, DETAIL_TOL_FRACTION * _bbox_diag(v))


def _face_areas(v, t):
    a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
    return 0.5 * np.linalg.norm(np.cross(b - a, c - a), axis=1)


def _face_normals(v, t):
    a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
    cr = np.cross(b - a, c - a)
    ln = np.linalg.norm(cr, axis=1)
    n = np.zeros_like(cr)
    ok = ln > 0.0
    n[ok] = cr[ok] / ln[ok, None]
    return n, ok


def _defect_face_mask(t):
    """Boundary/non-manifold faces, expanded by one vertex ring.

    Derived from the INPUT topology only: the repair must not define away its
    own damage. This is the healthy/defective split used by every factor.
    """
    t = np.asarray(t, dtype=np.int64)
    mask = np.zeros(len(t), dtype=bool)
    if len(t) == 0 or edge_table is None:
        return mask
    _edges, counts, inv = edge_table(t)
    half_face = np.tile(np.arange(len(t)), 3)
    mask[half_face[counts[inv] != 2]] = True
    if mask.any():
        bad_v = np.zeros(int(t.max()) + 1, dtype=bool)
        bad_v[t[mask].ravel()] = True
        mask |= bad_v[t].any(axis=1)
    return mask


def _area_samples(v, t, n_total, face_mask=None):
    """Deterministic area-weighted surface samples (points, normals, weights)."""
    t = np.asarray(t, dtype=np.int64)
    n = int(min(int(n_total), len(t)))
    if n <= 0 or len(t) == 0:
        return (np.zeros((0, 3)), np.zeros((0, 3)), np.zeros(0),
                np.zeros(0, dtype=np.int64))
    a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
    area = 0.5 * np.linalg.norm(np.cross(b - a, c - a), axis=1)
    total = float(area.sum())
    if total <= 0.0:
        return (np.zeros((0, 3)), np.zeros((0, 3)), np.zeros(0),
                np.zeros(0, dtype=np.int64))
    u_area = (np.arange(n, dtype=np.float64) + 0.5) / n * total
    cdf = np.cumsum(area)
    idx = np.clip(np.searchsorted(cdf, u_area, side='right'), 0, len(t) - 1)
    k = np.arange(n)
    u = np.mod((k + 0.5) * 0.7548776662466927, 1.0)
    w = np.mod((k + 0.5) * 0.5698402909980532, 1.0)
    flip = (u + w) > 1.0
    u[flip] = 1.0 - u[flip]
    w[flip] = 1.0 - w[flip]
    pts = (a[idx] * (1.0 - u - w)[:, None] + b[idx] * u[:, None]
           + c[idx] * w[:, None])
    normals, _ok = _face_normals(v, t)
    weights = np.full(n, total / n, dtype=np.float64)
    if face_mask is not None:
        keep = ~np.asarray(face_mask, dtype=bool)[idx]
        pts, normals, weights, idx = pts[keep], normals[keep], weights[keep], idx[keep]
    return pts, normals, weights, idx


def _winding(bvh, points, chunk=65536):
    out = np.empty(len(points), dtype=np.float64)
    for s in range(0, len(points), chunk):
        out[s:s + chunk] = np.asarray(
            bvh.winding_points(points[s:s + chunk]), dtype=np.float64)
    return out


def _signed_volume(v, t):
    a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
    return abs(float(np.einsum('ij,ij->i', a, np.cross(b, c)).sum() / 6.0))


def _sharp_edges(v, t, face_mask=None, angle_deg=SHARP_ANGLE_DEG):
    """(edges, dihedral_deg) above the angle threshold."""
    empty = (np.zeros((0, 2), dtype=np.int64), np.zeros(0))
    if len(t) == 0 or edge_table is None:
        return empty
    normals, valid_face = _face_normals(v, t)
    edges, counts, inv = edge_table(np.asarray(t, dtype=np.int64))
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
        fm = np.asarray(face_mask, dtype=bool)
        ok &= ~fm[f0] & ~fm[f1]
    dot = np.clip(np.einsum('ij,ij->i', normals[f0], normals[f1]), -1.0, 1.0)
    dih = np.degrees(np.arccos(dot))
    keep = ok & (dih > float(angle_deg))
    chosen = np.unique(eidx[keep])
    if len(chosen) == 0:
        return empty
    by_edge = np.full(len(edges), np.nan)
    by_edge[eidx[keep]] = dih[keep]
    return edges[chosen], by_edge[chosen]


def _sample_sharp(v, edges, dih, step, max_per_edge):
    """Deterministic stratified edge midpoints, length-weighted."""
    if len(edges) == 0:
        return np.zeros((0, 3)), np.zeros(0), np.zeros(0)
    lengths = np.linalg.norm(v[edges[:, 1]] - v[edges[:, 0]], axis=1)
    keep = lengths > 0.0
    if not keep.any():
        return np.zeros((0, 3)), np.zeros(0), np.zeros(0)
    edges, dih, lengths = edges[keep], dih[keep], lengths[keep]
    n = np.minimum(
        np.maximum(1, np.floor(lengths / max(float(step), 1e-12) + 0.5)
                   .astype(np.int64)), int(max_per_edge))
    idx = np.repeat(np.arange(len(edges)), n)
    starts = np.cumsum(n) - n
    k = np.arange(len(idx)) - np.repeat(starts, n)
    frac = (k + 0.5) / n[idx]
    pts = (v[edges[idx, 0]] * (1.0 - frac)[:, None]
           + v[edges[idx, 1]] * frac[:, None])
    return pts, dih[idx], lengths[idx] / n[idx]


def _feature_recall(vi, ti, vo, to, mask, tol, budget):
    in_e, in_d = _sharp_edges(vi, ti, face_mask=mask)
    if len(in_e) == 0:
        return None, {'note': 'no healthy sharp input edges'}
    out_e, out_d = _sharp_edges(vo, to)
    q, qd, qw = _sample_sharp(vi, in_e, in_d, tol,
                              budget['sharp_samples_per_edge'])
    if len(q) == 0:
        return None, {'note': 'no sharp-edge samples'}
    r, rd, _rw = _sample_sharp(vo, out_e, out_d, tol,
                               budget['sharp_samples_per_edge'])
    if len(r) == 0:
        return 0.0, {'note': 'no sharp output edges',
                     'input_sharp_edges': int(len(in_e))}
    if sutura_geom is None:
        return None, {'note': 'sutura_geom unavailable'}
    try:
        _dist, idx = sutura_geom.nearest_points(r, q, 1)
        dist = np.asarray(_dist, dtype=np.float64).reshape(-1)
        idx = np.asarray(idx, dtype=np.int64).reshape(-1)
    except Exception as e:  # noqa: BLE001
        return None, {'note': 'nearest_points failed: %s' % e}
    ok = (idx >= 0) & (dist <= tol)
    delta = np.full(len(q), np.inf)
    delta[ok] = np.abs(qd[ok] - rd[idx[ok]])
    keep = ok & (delta <= DIHEDRAL_MATCH_DEG)
    total = float(qw.sum())
    recall = float(qw[keep].sum() / total) if total > 0 else None
    return recall, {
        'input_sharp_edges': int(len(in_e)),
        'output_sharp_edges': int(len(out_e)),
        'samples': int(len(q)),
        'tol_mm': round(float(tol), 6),
        'recall': None if recall is None else round(recall, 6),
    }


def _healthy_fill(vi, ti, vo, to, mask, tol, budget, area, layers):
    if sutura_geom is None or len(ti) == 0 or len(to) == 0:
        return None
    try:
        base, normals, weight, _idx = _area_samples(
            vi, ti, int(area), face_mask=mask)
        if len(base) == 0:
            return None
        diag = _bbox_diag(vi)
        band = max(8.0 * tol, 0.01 * diag)
        layers = int(layers)
        if layers % 2:
            layers += 1
        layers = max(2, layers)
        delta = 2.0 * band / layers
        offsets = ((np.arange(layers, dtype=np.float64) + 0.5
                    - layers / 2.0) * delta)
        pts = (base[:, None, :] + normals[:, None, :] * offsets[None, :, None]
               ).reshape(-1, 3)
        vol_w = np.repeat(weight * delta, layers)
        bvh_in = sutura_geom.PyMeshBvh(vi, _as_i32(ti))
        bvh_out = sutura_geom.PyMeshBvh(vo, _as_i32(to))
        w_in = _winding(bvh_in, pts)
        w_out = _winding(bvh_out, pts)
        added = (w_out > 0.5) & (w_in < 0.5)
        removed = (w_in > 0.5) & (w_out < 0.5)
        fill = float(vol_w[added].sum())
        rem = float(vol_w[removed].sum())
        try:
            basis_vol = _signed_volume(vi, ti)
        except Exception:  # noqa: BLE001
            basis_vol = 0.0
        if not basis_vol > 0.0:
            lo, hi = vi.min(0), vi.max(0)
            basis_vol = float(np.prod(np.maximum(hi - lo, 1e-12)))
        out = {
            'fill_mm3': round(fill, 6),
            'removed_mm3': round(rem, 6),
            'fill_fraction': None,
            'removed_fraction': None,
            'samples': int(len(pts)),
        }
        if basis_vol > 0.0:
            out['fill_fraction'] = float(fill / basis_vol)
            out['removed_fraction'] = float(rem / basis_vol)
        return out
    except Exception as e:  # noqa: BLE001
        return {'note': 'healthy volume failed: %s' % e}


def _union_areas(t, flagged, area, min_area):
    """Count edge-connected components of flagged faces above ``min_area``."""
    n = len(t)
    parent = np.arange(n, dtype=np.int64)

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    try:
        _edges, counts, inv = edge_table(np.asarray(t, dtype=np.int64))
    except Exception:  # noqa: BLE001
        return None
    half_face = np.tile(np.arange(n), 3)
    sel = counts[inv] == 2
    eids = inv[sel]
    faces = half_face[sel]
    order = np.argsort(eids, kind='stable')
    eids, faces = eids[order], faces[order]
    starts = np.flatnonzero(np.r_[True, eids[1:] != eids[:-1]])
    f0 = faces[starts]
    f1 = faces[starts + 1]
    for a, b in zip(f0.tolist(), f1.tolist()):
        if flagged[a] and flagged[b]:
            ra, rb = find(a), find(b)
            if ra != rb:
                parent[rb] = ra
    comp_area = {}
    for i in np.flatnonzero(flagged).tolist():
        r = find(i)
        comp_area[r] = comp_area.get(r, 0.0) + float(area[i])
    total = float(area.sum())
    if total <= 0.0:
        return None
    return sum(1 for a in comp_area.values() if a >= min_area * total)


def _bbox_change_pct(vi, vo):
    if len(vi) == 0 or len(vo) == 0:
        return None
    a = vi.max(0) - vi.min(0)
    b = vo.max(0) - vo.min(0)
    da = float(np.linalg.norm(a))
    if da <= 0.0:
        return None
    db = float(np.linalg.norm(b))
    return abs(db - da) / da * 100.0


def _health_tier(health, cfg):
    tiers = (cfg.get('status') or {}).get('health_tiers') or {}
    mid = float(tiers.get('mid', 60))
    return health >= mid


def _verdict(score, report, low_confidence, reasons, areas, cfg):
    try:
        import repair_score
        _rs = repair_score.compute_scores(report, cfg)
        health = _rs.get('health')
    except Exception:  # noqa: BLE001
        health = None
    low_areas = max(int(areas or 0), 1 if low_confidence else 0)
    if health is not None and not _health_tier(health, cfg):
        return 'not_repaired', low_areas
    if score is None:
        return 'unavailable', low_areas
    if low_confidence:
        return 'low_confidence', low_areas
    if areas:
        return 'changed', low_areas
    high = float((cfg.get('fidelity') or {}).get('tiers', {}).get('high', 85))
    if score >= high:
        return 'preserved', low_areas
    return 'changed', max(low_areas, 1)


def compute(in_arrays, out_arrays, report=None, config=None):
    """Compute Fidelity and the plain verdict for a finished repair.

    ``in_arrays`` / ``out_arrays`` are ``(verts, tris)``. Returns a flat dict
    ready to merge into the repair report; never raises.
    """
    empty = {
        'repair_fidelity': None,
        'repair_fidelity_factors': {},
        'repair_fidelity_note': 'not measured',
        'repair_verdict_code': 'unavailable',
        'repair_verdict_areas': 0,
        'repair_verdict_low_confidence': False,
        'repair_verdict_reasons': [],
    }
    try:
        cfg = load_config(config)
        fc = cfg.get('fidelity') or {}
        report = report or {}
        if sutura_geom is None or edge_table is None:
            empty['repair_fidelity_note'] = 'sutura_geom unavailable'
            return empty
        vi, ti = _as_f64(in_arrays[0]), _as_i32(in_arrays[1])
        vo, to = _as_f64(out_arrays[0]), _as_i32(out_arrays[1])
        if vi.size == 0 or ti.size == 0 or vo.size == 0 or to.size == 0:
            empty['repair_fidelity_note'] = 'empty mesh'
            return empty

        tol = _detail_tol(vi)
        diag = _bbox_diag(vi)
        budgets = fc.get('budgets') or _DEFAULTS['fidelity']['budgets']
        reduced = len(to) > int(budgets.get('full_faces', 20000))
        gated = (len(to) > int(budgets.get('max_faces', 150000))
                 or len(ti) > int(budgets.get('max_faces', 150000)))
        note = []
        if gated:
            note.append('large mesh: reduced budget')
        elif reduced:
            note.append('reduced budget')

        mask = _defect_face_mask(ti)

        # --- one distance query, reused by distance / invented area / areas
        area = _face_areas(vo, to)
        if not reduced:
            pts, w, face_idx = ((vo[to[:, 0]] + vo[to[:, 1]] + vo[to[:, 2]])
                                / 3.0, area, np.arange(len(to)))
        else:
            pts, _normals, w, face_idx = _area_samples(
                vo, to, int(budgets.get('reduced_distance_samples', 1200)))
            area = w
        if len(pts) == 0:
            empty['repair_fidelity_note'] = 'no samples'
            return empty
        _q, d, nf = sutura_geom.closest_points(vi, _as_i32(ti), pts)
        d = np.asarray(d, dtype=np.float64)
        nf = np.asarray(nf, dtype=np.int64)
        healthy = ~mask[np.clip(nf, 0, max(len(mask) - 1, 0))] if len(mask) else \
            np.ones(len(d), dtype=bool)
        wh = w[healthy]
        dh = d[healthy]
        if wh.sum() > 0.0:
            max_pct = float(dh.max()) / diag * 100.0 if diag > 0 else None
            mean_pct = float((dh * wh).sum() / wh.sum()) / diag * 100.0 \
                if diag > 0 else None
        else:
            max_pct = mean_pct = None
        invented_frac = float(w[d > tol].sum() / w.sum()) if w.sum() > 0 else 0.0

        # --- changed / low-confidence areas (full budget only)
        areas_n = None
        low_areas = 0
        if not reduced:
            flagged = d > (tol * float(fc.get('change_tol_factor', 3.0)))
            min_frac = float((fc.get('low_confidence') or {})
                             .get('area_min_fraction', 0.0005))
            max_areas = int((fc.get('low_confidence') or {})
                            .get('max_areas', 99))
            areas_n = _union_areas(to, flagged, area, min_frac)
            if areas_n is not None:
                areas_n = min(int(areas_n), max_areas)
            low_areas = _union_areas(
                to, flagged & (d > tol), area,
                float((fc.get('low_confidence') or {})
                      .get('low_area_min_fraction', 0.03))) or 0
            low_areas = min(int(low_areas), max_areas)

        # --- feature recall (gated: full edge table on a very large mesh is
        # the most expensive part and scan noise makes it less informative)
        recall_max = int(budgets.get('recall_max_faces', 50000))
        if gated or max(len(ti), len(to)) > recall_max:
            recall, rec_detail = None, {'note': 'feature recall skipped '
                                                '(large mesh)'}
        else:
            rec_budget = dict(budgets)
            if reduced:
                rec_budget['sharp_samples_per_edge'] = int(
                    budgets.get('reduced_sharp_samples_per_edge', 2))
            recall, rec_detail = _feature_recall(
                vi, ti, vo, to, mask, tol, rec_budget)

        # --- healthy volume
        fill = None
        if not gated:
            area_n = budgets.get('full_lattice_area', 1200)
            lay = budgets.get('full_lattice_layers', 6)
            if reduced:
                area_n = budgets.get('reduced_lattice_area', 400)
                lay = budgets.get('reduced_lattice_layers', 4)
            fill = _healthy_fill(vi, ti, vo, to, mask, tol,
                                 budgets, area_n, lay)

        # --- cheap deltas
        s1 = report.get('stage1') or {}
        vol_pct = s1.get('volume_change_percent')
        if not isinstance(vol_pct, (int, float)) or isinstance(vol_pct, bool):
            vol_pct = None
        bbox_pct = _bbox_change_pct(vi, vo)

        factors = {}
        used = 0.0
        num = 0.0
        weights = fc.get('weights') or _DEFAULTS['fidelity']['weights']

        def add(name, penalty, detail):
            nonlocal used, num
            w = max(float(weights.get(name, 0.0)), 0.0)
            if w <= 0:
                return
            pen = max(0.0, min(1.0, float(penalty)))
            used += w
            num += w * pen
            factors[name] = {'penalty': round(pen, 4), 'weight': round(w, 2),
                             'detail': detail}

        if max_pct is not None:
            scale = float(fc.get('distance_scale_pct', 0.5)) or 0.5
            mw = float(fc.get('distance_mean_weight', 0.3))
            pen = (1.0 - mw) * min(1.0, max_pct / scale) \
                + mw * min(1.0, (mean_pct or 0.0) / scale)
            add('healthy_distance', pen,
                {'max_pct': round(max_pct, 4),
                 'mean_pct': None if mean_pct is None else round(mean_pct, 4),
                 'samples': int(healthy.sum())})
        if recall is not None:
            add('feature_recall', 1.0 - float(recall), rec_detail)
        if fill and fill.get('fill_fraction') is not None:
            vscale = float(fc.get('volume_scale_fraction', 0.01)) or 0.01
            fr = float(fill.get('fill_fraction') or 0.0)
            rr = float(fill.get('removed_fraction') or 0.0)
            add('healthy_volume', min(1.0, (fr + rr) / vscale),
                {'fill_fraction': round(fr, 6),
                 'removed_fraction': round(rr, 6),
                 'samples': fill.get('samples')})
        if vol_pct is not None or bbox_pct is not None:
            vscale = float(fc.get('volume_change_scale_pct', 15.0)) or 15.0
            bscale = float(fc.get('bbox_scale_pct', 5.0)) or 5.0
            vp = min(1.0, abs(float(vol_pct or 0.0)) / vscale)
            bp = min(1.0, abs(float(bbox_pct or 0.0)) / bscale)
            add('volume_bbox', max(vp, bp),
                {'volume_change_pct': None if vol_pct is None
                 else round(float(vol_pct), 4),
                 'bbox_change_pct': None if bbox_pct is None
                 else round(float(bbox_pct), 4)})
        iscale = float(fc.get('invented_scale_fraction', 0.05)) or 0.05
        add('invented_area', min(1.0, invented_frac / iscale),
            {'fraction': round(invented_frac, 6)})

        score = None
        if max_pct is None:
            note.append('no healthy distance samples')
        if recall is None:
            note.append('feature recall unavailable')
        if fill is None:
            note.append('healthy volume unavailable')
        if used > 0.0:
            score = int(round(max(0.0, min(1.0, 1.0 - num / used)) * 100))

        # --- low-confidence evidence
        reasons = []
        for tier_name, flag in (
                ('ftetwild', 'experimental_ftetwild'),
                ('splint', 'splint')):
            tier = report.get(tier_name) or report.get(flag) or {}
            if isinstance(tier, dict) and tier.get('adopted'):
                reasons.append(tier_name)
        lc = fc.get('low_confidence') or {}
        if invented_frac >= float(lc.get('invented_area_fraction', 0.03)):
            reasons.append('invented_area')
        if low_areas:
            reasons.append('changed_areas')
        low_confidence = bool(reasons)

        code, verdict_areas = _verdict(
            score, report, low_confidence, reasons,
            areas_n if areas_n is not None else low_areas, cfg)
        if note:
            empty['repair_fidelity_note'] = '; '.join(note)

        return {
            'repair_fidelity': score,
            'repair_fidelity_factors': factors,
            'repair_fidelity_note': '; '.join(note) if note else None,
            'repair_fidelity_metrics': {
                'healthy_distance_max_pct': None if max_pct is None
                else round(max_pct, 4),
                'healthy_distance_mean_pct': None if mean_pct is None
                else round(mean_pct, 4),
                'feature_recall': recall,
                'healthy_fill_fraction': None if not fill
                else fill.get('fill_fraction'),
                'healthy_removed_fraction': None if not fill
                else fill.get('removed_fraction'),
                'invented_area_fraction': round(invented_frac, 6),
                'changed_areas': areas_n,
                'low_confidence_areas': low_areas,
                'budget': 'gated' if gated else ('reduced' if reduced else 'full'),
            },
            'repair_verdict_code': code,
            'repair_verdict_areas': int(verdict_areas),
            'repair_verdict_low_confidence': low_confidence,
            'repair_verdict_reasons': reasons,
        }
    except Exception as e:  # noqa: BLE001 - reporting must never break repair
        empty['repair_fidelity_note'] = 'fidelity failed: %s' % e
        return empty


def attach_file(result, src, out):
    """Load ``src``/``out`` meshes and attach fidelity to the final report.

    Multi-object 3MF: one measurement per object, then an aggregate (worst
    object for the axis, summed areas). Fail-silent.
    """
    try:
        if not isinstance(result, dict) or 'error' in result or 'stage1' not in result:
            return result
        from sutura_engine.pipeline.mesh_io import load_meshes
        ins = load_meshes(src)
        outs = load_meshes(out)
        if not ins or not outs:
            return result
        reports = result.get('object_reports')
        if reports:
            n = min(len(reports), len(ins), len(outs))
            vals = []
            for i in range(n):
                vals.append(compute((ins[i][1], ins[i][2]),
                                    (outs[i][1], outs[i][2]), reports[i]))
            for i, v in enumerate(vals):
                reports[i].update(v)
            agg = _aggregate(vals)
            if agg:
                result.update(agg)
        else:
            result.update(compute((ins[0][1], ins[0][2]),
                                  (outs[0][1], outs[0][2]), result))
    except Exception as e:  # noqa: BLE001
        try:
            result.setdefault('repair_fidelity_note', 'fidelity failed: %s' % e)
        except Exception:
            pass
    return result


def _aggregate(vals):
    vals = [v for v in vals if isinstance(v, dict)]
    if not vals:
        return None
    scores = [v.get('repair_fidelity') for v in vals
              if isinstance(v.get('repair_fidelity'), (int, float))]
    metrics = [v.get('repair_fidelity_metrics') or {} for v in vals]
    worst = min(vals, key=lambda v: (v.get('repair_fidelity') is None,
                                     v.get('repair_fidelity') or 100))
    changed = [m.get('changed_areas') for m in metrics
               if isinstance(m.get('changed_areas'), (int, float))]
    low = [m.get('low_confidence_areas') for m in metrics
           if isinstance(m.get('low_confidence_areas'), (int, float))]
    reasons = sorted({r for v in vals for r in (v.get('repair_verdict_reasons') or [])})
    return {
        'repair_fidelity': int(min(scores)) if scores else None,
        'repair_fidelity_factors': worst.get('repair_fidelity_factors') or {},
        'repair_fidelity_note': worst.get('repair_fidelity_note'),
        'repair_fidelity_metrics': {
            'worst_object': int(vals.index(worst)),
            'changed_areas': int(sum(changed)) if changed else None,
            'low_confidence_areas': int(sum(low)) if low else None,
        },
        'repair_verdict_code': worst.get('repair_verdict_code', 'unavailable'),
        'repair_verdict_areas': int(sum(
            v.get('repair_verdict_areas') or 0 for v in vals)),
        'repair_verdict_low_confidence': any(
            v.get('repair_verdict_low_confidence') for v in vals),
        'repair_verdict_reasons': reasons,
    }
