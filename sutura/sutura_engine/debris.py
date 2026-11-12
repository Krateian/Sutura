# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Read-only small-part / possible-debris measurement for the repair report.

Nothing here changes geometry: it only measures (1) the components the
Stage-1 ``<min_faces`` removal drops, and (2) which surviving components look
like possible debris by the census rule F. Pure numpy + ``topology``.

Rules and evidence come from a read-only debris census of the benchmark
corpus. Components are vertex-connected faces,
exactly what ``diagnosis::part_report`` uses.

TODO(rust): port rule F into ``sutura_geom``'s diagnosis ``part_report`` so the
signal has one source; kept in Python here because extending the Rust report
needs new plumbing.
"""
import numpy as np

MAX_REPORTED = 32              # capped per-component list in the report
MAX_DISTANCE_SAMPLES = 20000   # closest_points query budget per component

# Census rule F (conservative "possible debris" flag, report only).
TAU_VOL = 0.005                # |vol(C)| < 0.5% of the largest |vol|
ZERO_VOL_REL = 1e-9            # |vol(C)| <= 1e-9 * gdiag^3
DEGENERATE_REL = 1e-9          # area(C) <= 1e-9 * gdiag^2


def _global_diag(verts):
    v = np.asarray(verts, dtype=np.float64)
    if v.size == 0:
        return 0.0
    return float(np.linalg.norm(v.max(axis=0) - v.min(axis=0)))


def _components(verts, tris):
    """Vertex-connected face components: (labels_per_face, n_components)."""
    import topology

    t = np.asarray(tris, dtype=np.int64)
    v = np.asarray(verts, dtype=np.float64)
    if len(t) == 0 or len(v) == 0:
        return np.zeros(len(t), dtype=np.int64), 0
    e = np.vstack([t[:, [0, 1]], t[:, [1, 2]], t[:, [2, 0]]])
    n_comp, labels = topology.connected_components((len(v), e))
    return labels[t[:, 0]], int(n_comp)


def _face_metrics(verts, tris):
    """Per-face area and signed tetra volume (about the origin)."""
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
    area = 0.5 * np.linalg.norm(np.cross(b - a, c - a), axis=1)
    vol = np.einsum('ij,ij->i', a, np.cross(b, c)) / 6.0
    return area, vol


def _edge_open(faces):
    """True when any edge of this face subset is not shared by exactly 2 faces."""
    if len(faces) == 0:
        return False
    edges = np.vstack([faces[:, [0, 1]], faces[:, [1, 2]], faces[:, [2, 0]]])
    edges = np.sort(edges, axis=1)
    order = np.lexsort((edges[:, 1], edges[:, 0]))
    se = edges[order]
    changed = np.ones(len(se), dtype=bool)
    changed[1:] = (se[1:, 0] != se[:-1, 0]) | (se[1:, 1] != se[:-1, 1])
    starts = np.nonzero(changed)[0]
    ends = np.append(starts[1:], len(se))
    return bool(np.any((ends - starts) != 2))


def _subset(verts, tris, face_idx):
    """(verts, tris) of just these faces, with referenced vertices re-indexed."""
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    sub = t[face_idx]
    if len(sub) == 0:
        return np.zeros((0, 3), dtype=np.float64), np.zeros((0, 3), dtype=np.int64)
    used = np.unique(sub)
    remap = np.full(len(v), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return v[used], remap[sub]


def _distance_to_main(verts, tris, query_vids, main_face_idx):
    """Nearest vertex-to-surface distance from a component to the main body.

    Uses the shipped ``sutura_geom.closest_points`` (deterministic BVH); the
    query vertices are capped with a fixed sample so the cost is bounded.
    Returns None when the extension is unavailable.
    """
    try:
        import sutura_geom
    except Exception:
        return None
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    if len(query_vids) == 0 or len(main_face_idx) == 0:
        return None
    mv, mt = _subset(v, t, main_face_idx)
    if len(mt) == 0:
        return None
    pts = v[np.asarray(query_vids, dtype=np.int64)]
    if len(pts) > MAX_DISTANCE_SAMPLES:
        idx = np.linspace(0, len(pts) - 1, MAX_DISTANCE_SAMPLES).astype(np.int64)
        pts = pts[idx]
    try:
        _q, d, _f = sutura_geom.closest_points(mv, mt, pts)
        d = np.asarray(d, dtype=np.float64)
        if d.size == 0 or not np.isfinite(d).all():
            return None
        return round(float(d.min()), 4)
    except Exception:
        return None


def _tables(verts, tris):
    labels, n_comp = _components(verts, tris)
    if n_comp == 0:
        return None
    area, vol = _face_metrics(verts, tris)
    faces = np.bincount(labels, minlength=n_comp).astype(np.int64)
    vols = np.bincount(labels, weights=vol, minlength=n_comp)
    areas = np.bincount(labels, weights=area, minlength=n_comp)
    largest = int(np.argmax(np.abs(vols))) if n_comp else -1
    return {'labels': labels, 'n_comp': n_comp, 'faces': faces, 'vols': vols,
            'areas': areas, 'largest': largest}


def _comp_faces(labels, c):
    return np.nonzero(labels == c)[0]


def _bvid(tris, face_idx):
    if len(face_idx) == 0:
        return np.zeros(0, dtype=np.int64)
    return np.unique(np.asarray(tris)[face_idx].reshape(-1))


def removed_small_parts(verts, tris, min_faces, cap=MAX_REPORTED):
    """Components with fewer than ``min_faces`` triangles (the Stage-1 drop).

    Returns ``{'count', 'faces_total', 'capped', 'parts': [...]}`` or None when
    nothing is removed. Each part carries face count, bounding-box size (mm and
    % of the model) and the distance to the main (largest) body.
    """
    tab = _tables(verts, tris)
    if tab is None:
        return None
    # Ignore vertex-only components (unreferenced vertices left by an
    # earlier filter): they have 0 faces and must not appear as 'removed
    # small parts'.
    small = [c for c in range(tab['n_comp'])
             if 0 < int(tab['faces'][c]) < min_faces]
    if not small:
        return None
    gdiag = _global_diag(verts)
    main_faces = _comp_faces(tab['labels'], tab['largest']) if tab['largest'] >= 0 else np.zeros(0, dtype=np.int64)
    t = np.asarray(tris, dtype=np.int64)
    v = np.asarray(verts, dtype=np.float64)
    small.sort(key=lambda c: (-int(tab['faces'][c]),))
    parts = []
    for c in small[:cap]:
        f_idx = _comp_faces(tab['labels'], c)
        vids = _bvid(t, f_idx)
        pts = v[vids] if len(vids) else np.zeros((0, 3))
        size = float(np.linalg.norm(pts.max(axis=0) - pts.min(axis=0))) if len(pts) else 0.0
        parts.append({
            'faces': int(tab['faces'][c]),
            'size_mm': round(size, 4),
            'size_pct': round(100.0 * size / gdiag, 3) if gdiag > 0 else None,
            'distance_mm': _distance_to_main(verts, tris, vids, main_faces),
        })
    return {'count': len(small), 'min_faces': int(min_faces),
            'faces_total': int(sum(int(tab['faces'][c]) for c in small)),
            'capped': len(small) > cap, 'parts': parts}


def possible_debris(verts, tris, min_faces=8, cap=MAX_REPORTED, tau=TAU_VOL):
    """Census rule F: separate small closed parts that look like possible debris.

    Report-only: never deletes. Returns ``{'count', 'capped', 'parts': [...]}``
    or None. Components with fewer than ``min_faces`` triangles are excluded --
    those are the ones the Stage-1 ``<min_faces`` removal already reports.
    """
    tab = _tables(verts, tris)
    if tab is None or tab['n_comp'] <= 1:
        return None
    gdiag = _global_diag(verts)
    if gdiag <= 0:
        return None
    max_vol = float(np.max(np.abs(tab['vols'])))
    if max_vol <= 0:
        return None
    zero_vol = ZERO_VOL_REL * gdiag ** 3
    degenerate = DEGENERATE_REL * gdiag ** 2
    t = np.asarray(tris, dtype=np.int64)
    v = np.asarray(verts, dtype=np.float64)
    main_faces = _comp_faces(tab['labels'], tab['largest'])
    flagged = []
    for c in range(tab['n_comp']):
        if c == tab['largest'] or tab['faces'][c] < min_faces:
            continue
        if abs(tab['vols'][c]) <= zero_vol or tab['areas'][c] <= degenerate:
            continue
        if abs(tab['vols'][c]) >= tau * max_vol:
            continue
        f_idx = _comp_faces(tab['labels'], c)
        if _edge_open(t[f_idx]):
            continue
        vids = _bvid(t, f_idx)
        pts = v[vids]
        size = float(np.linalg.norm(pts.max(axis=0) - pts.min(axis=0)))
        flagged.append({
            'faces': int(tab['faces'][c]),
            'volume_pct': float(round(100.0 * abs(float(tab['vols'][c])) / max_vol, 4)),
            'size_pct': float(round(100.0 * size / gdiag, 3)),
            'distance_mm': _distance_to_main(verts, tris, vids, main_faces),
        })
    if not flagged:
        return None
    flagged.sort(key=lambda p: -p['volume_pct'])
    return {'count': len(flagged), 'capped': len(flagged) > cap,
            'parts': flagged[:cap]}
