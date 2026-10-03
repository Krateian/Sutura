#!/usr/bin/env python3
"""Rust/numpy parity test for the Flap hole-filler port.

``sutura_geom.flap_fill`` (rust/sutura-geom/src/flap) is the Rust port of the
numpy oracle in ``sutura_engine.flap``. The port is exposed through
``sutura_engine.flap.flap_fill`` (feature-detected, numpy fallback). This test
pins the contract the port must keep: on a synthetic holed mesh the two engines
produce the SAME triangles and vertices within 1e-8.

Skips (exit 0) when the extension is absent or predates the binding, so
installs without the Rust build still pass.

Needs the venv (trimesh/scipy). pytest-compatible; runnable directly:
    ~/.local/share/sutura/venv/bin/python tests/test_flap_rust_parity.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for p in (SUTURA, REPO):
    if p not in sys.path:
        sys.path.insert(0, p)

import numpy as np  # noqa: E402
from scipy.spatial import cKDTree  # noqa: E402

from sutura_engine import flap  # noqa: E402

try:
    import pytest  # noqa: E402
except ImportError:  # pragma: no cover - direct-script runs may lack pytest
    pytest = None


def _require_rust():
    rfn = flap._load_rust_flap()
    if rfn is None:
        msg = ('sutura_geom.flap_fill unavailable (extension absent or '
               'predates the binding) - skipping parity test')
        if pytest is not None:
            pytest.skip(msg)
        print('SKIP: ' + msg)
        sys.exit(0)
    return rfn


def _holed_sphere(subdiv=4, radius=5.0, cut=4.0):
    import trimesh
    m = trimesh.creation.icosphere(subdivisions=subdiv, radius=radius)
    v = np.asarray(m.vertices, np.float64)
    t = np.asarray(m.faces, np.int64)
    c = v[t].mean(axis=1)
    return v, t[c[:, 2] < cut]


def _assert_same_mesh(v_py, t_py, v_rs, t_rs, tol=1e-8):
    assert v_rs.shape[0] == v_py.shape[0], (v_rs.shape, v_py.shape)
    assert t_rs.shape[0] == t_py.shape[0], (t_rs.shape, t_py.shape)
    # Rust vertices must coincide with the numpy ones within tol, one-to-one,
    # so the two meshes are the same geometry with a (possibly) permuted
    # vertex numbering.
    dist, idx = cKDTree(v_py).query(v_rs)
    assert dist.max() <= tol, 'vertex deviation %.3g > %.3g' % (dist.max(), tol)
    assert len(set(idx.tolist())) == v_rs.shape[0], 'vertex map not one-to-one'
    remapped = np.asarray(idx, np.int64)[np.asarray(t_rs, np.int64)]
    assert _face_set(remapped) == _face_set(t_py), 'face sets differ'


def _face_set(tris):
    """Orientation-insensitive set of triangles (sorted vertex triples)."""
    t = np.asarray(tris, np.int64)
    return set(map(tuple, np.sort(t, axis=1).tolist()))


def test_rust_matches_python_on_holed_mesh():
    rfn = _require_rust()
    v, t = _holed_sphere()
    pv, pt, prep = flap.flap_fill_python(v, t, separate_stl=False)
    # Call the Rust binding directly (not the dispatcher) so the comparison
    # cannot pass via a silent numpy fallback.
    rv, rt, rrep = rfn(v, t, separate_stl=False)
    _assert_same_mesh(pv, pt, np.asarray(rv), np.asarray(rt))
    for key in ('loops_found', 'loops_filled', 'patch_faces', 'new_vertices'):
        assert int(rrep[key]) == int(prep[key]), (key, rrep.get(key), prep.get(key))
    assert prep['loops_found'] == 1 and prep['patch_faces'] > 0, prep
    # The public dispatcher must route to the same Rust result.
    dv, dt, _ = flap.flap_fill(v, t, separate_stl=False)
    _assert_same_mesh(pv, pt, np.asarray(dv), np.asarray(dt))


def test_rust_matches_python_noop_on_closed_mesh():
    rfn = _require_rust()
    import trimesh
    m = trimesh.creation.box(extents=(10.0, 10.0, 10.0))
    v = np.asarray(m.vertices, np.float64)
    t = np.asarray(m.faces, np.int64)
    pv, pt, prep = flap.flap_fill_python(v, t, separate_stl=False)
    rv, rt, rrep = rfn(v, t, separate_stl=False)
    _assert_same_mesh(pv, pt, np.asarray(rv), np.asarray(rt))
    assert prep['loops_found'] == 0 and rrep['loops_found'] == 0


if __name__ == '__main__':
    _require_rust()
    fns = [v for k, v in sorted(globals().items()) if k.startswith('test_')]
    for fn in fns:
        fn()
        print('ok: %s' % fn.__name__)
    print('all %d flap Rust-parity tests passed' % len(fns))
