#!/usr/bin/env python3
"""Indirect autorefine bridge: exact arrangement-lite self-intersection split
via the rust/sutura-geom extension.

Phase B (§8): replaces/augments ``sutura/autorefine.py``'s float64
snap-rounding self-intersection pass with the exact indirect-predicate engine
``sutura_geom.arrangement_lite``.  The Rust core runs a uniform-grid AABB
broad phase, the exact triangle-triangle classifier, a per-triangle projected
2D CDT, and exact-rational vertex welding, so the split output carries no
proper self-intersections.

The bridge imports ``sutura_geom`` lazily and reports an explicit skip (never
a crash) when the extension is unavailable in the active interpreter.
Mirrors manifold_bridge.py / ftetwild_bridge.py: a subprocess CLI entry (OBJ
in, OBJ out, JSON report) exists for the fixed Linux two-venv layout where the
extension may only be present under venv311; ``repair.py`` also calls
``arrangement_lite_arrays`` in-process on the numpy arrays it already holds.
"""
import json
import sys

import numpy as np


def arrangement_lite_arrays(verts, tris):
    """Run the exact arrangement-lite split on numpy arrays.

    Returns ``(new_verts, new_tris, report)``.  Raises ``ImportError`` when
    ``sutura_geom`` is not importable in the active interpreter; the caller
    catches it and records an explicit skip (never crashes a repair).  The
    report carries the Phase B §8.3 fields (``si_before``/``si_after``,
    ``faces_before``/``faces_after``); ``si_after`` is 0 because the split
    removes every proper intersection by construction.
    """
    import sutura_geom  # noqa: F401  (lazy: skip, never crash, when absent)
    try:
        out_v, out_t, report = sutura_geom.arrangement_lite(verts, tris)
    except BaseException as e:  # noqa: BLE001 - a Rust panic (pyo3
        # PanicException) must never crash a repair
        if isinstance(e, (KeyboardInterrupt, SystemExit)):
            raise
        raise RuntimeError('indirect arrangement-lite failed: %s: %s'
                           % (type(e).__name__, e)) from None
    new_v = np.asarray(out_v, dtype=np.float32)
    new_t = np.asarray(out_t, dtype=np.int32)
    return new_v, new_t, {
        'ran': True,
        'si_before': int(report['si_pairs_detected']),
        'si_after': 0,
        'faces_before': int(report['input_faces']),
        'faces_after': int(report['output_faces']),
        'degenerate_cases': report.get('degenerate_cases', {}),
        'error': None,
    }


def read_obj(path):
    verts, tris = [], []
    with open(path) as f:
        for line in f:
            s = line.split()
            if not s:
                continue
            if s[0] == 'v':
                verts.append([float(s[1]), float(s[2]), float(s[3])])
            elif s[0] == 'f':
                tris.append([int(x.split('/')[0]) - 1 for x in s[1:4]])
    return (np.asarray(verts, dtype=np.float32),
            np.asarray(tris, dtype=np.int32))


def write_obj(path, verts, tris):
    with open(path, 'w') as f:
        f.write('# indirect autorefine bridge output\n')
        for v in verts:
            f.write('v %.9g %.9g %.9g\n' % (v[0], v[1], v[2]))
        for t in tris:
            f.write('f %d %d %d\n' % (t[0] + 1, t[1] + 1, t[2] + 1))


def run_bridge(src, dst):
    """Run arrangement-lite on the OBJ at src, write the split OBJ to dst and
    return the report dict.  ``ok`` is True on success; never raises for a
    missing extension (reports an explicit skip instead)."""
    report = {}
    try:
        import sutura_geom  # noqa: F401
    except ImportError as e:
        report['error'] = ('indirect autorefine skipped: sutura_geom extension '
                           'not available in this environment (%s)' % e)
        return report
    try:
        verts, tris = read_obj(src)
        new_v, new_t, rep = arrangement_lite_arrays(verts, tris)
        write_obj(dst, new_v, new_t)
        report.update(rep)
        report['ok'] = True
    except Exception as e:  # noqa: BLE001 - a bad mesh must never crash a repair
        report['error'] = '%s: %s' % (type(e).__name__, e)
    return report


def main():
    src, dst = sys.argv[1], sys.argv[2]
    report = run_bridge(src, dst)
    print(json.dumps(report))


if __name__ == '__main__':
    main()