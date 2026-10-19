#!/usr/bin/env python3
"""Golden-fixture regression for the Flap hole-filler Rust core.

``sutura_geom.flap_fill`` (rust/sutura-geom/src/flap) is the only Flap
implementation; the pure-Python oracle has been removed. This test pins the
contract against a committed fixture generated once from that oracle (before
its deletion) with ``SUTURA_FLAP_SELECT=0``: on a synthetic holed mesh and a
closed box, the Rust binding and the ``sutura_engine.flap.flap_fill``
dispatcher must reproduce the stored vertices and triangles within 1e-8, keep
the one-to-one vertex map and the report keys.

The extension is required, so a missing ``sutura_geom`` FAILS this test
instead of skipping it.

Needs the venv (trimesh). pytest-compatible; runnable directly:
    ~/.local/share/sutura/venv/bin/python tests/test_flap_rust_parity.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for p in (SUTURA, REPO):
    if p not in sys.path:
        sys.path.insert(0, p)

# The per-loop candidate selector (SUTURA_FLAP_SELECT) is a Rust-only extension
# with no oracle counterpart: it never changes a loop the thin-plate primary
# fills, but it can fill loops the oracle leaves to Stage-1. Pin it off so the
# fixture keeps verifying the pure port, not the selector.
os.environ['SUTURA_FLAP_SELECT'] = '0'

import numpy as np  # noqa: E402

from sutura_engine import flap  # noqa: E402

FIXTURE = os.path.join(REPO, 'tests', 'fixtures', 'flap_golden.npz')


def _require_rust():
    rfn = flap._load_rust_flap()
    if rfn is None:
        raise AssertionError(
            'sutura_geom.flap_fill is required (the Python oracle was removed); '
            'install the Rust extension')
    return rfn


def _fixture():
    with np.load(FIXTURE) as data:
        return {k: np.asarray(data[k]) for k in data.files}


def _assert_same_mesh(v_py, t_py, v_rs, t_rs, tol=1e-8):
    assert v_rs.shape[0] == v_py.shape[0], (v_rs.shape, v_py.shape)
    assert t_rs.shape[0] == t_py.shape[0], (t_rs.shape, t_py.shape)
    # Rust vertices must coincide with the stored ones within tol, one-to-one,
    # so the two meshes are the same geometry with a (possibly) permuted
    # vertex numbering.
    d2 = ((v_rs[:, None, :] - v_py[None, :, :]) ** 2).sum(axis=2)
    idx = np.argmin(d2, axis=1)
    dist = np.sqrt(d2[np.arange(len(v_rs)), idx])
    assert dist.max() <= tol, 'vertex deviation %.3g > %.3g' % (dist.max(), tol)
    assert len(set(idx.tolist())) == v_rs.shape[0], 'vertex map not one-to-one'
    remapped = np.asarray(idx, np.int64)[np.asarray(t_rs, np.int64)]
    assert _face_set(remapped) == _face_set(t_py), 'face sets differ'


def _face_set(tris):
    """Orientation-insensitive set of triangles (sorted vertex triples)."""
    t = np.asarray(tris, np.int64)
    return set(map(tuple, np.sort(t, axis=1).tolist()))


def _check_case(name, rfn, data):
    v = data[name + '_in_v']
    t = data[name + '_in_t']
    exp_v = data[name + '_out_v']
    exp_t = data[name + '_out_t']
    # Call the Rust binding directly (not the dispatcher) so the comparison
    # cannot pass through any other path.
    rv, rt, rrep = rfn(v, t, separate_stl=False)
    _assert_same_mesh(exp_v, exp_t, np.asarray(rv), np.asarray(rt))
    for key in ('loops_found', 'loops_filled', 'patch_faces', 'new_vertices'):
        assert int(rrep[key]) == int(data[name + '_' + key]), (
            key, rrep.get(key), int(data[name + '_' + key]))
    # The public dispatcher must route to the same Rust result.
    dv, dt, _ = flap.flap_fill(v, t, separate_stl=False)
    _assert_same_mesh(exp_v, exp_t, np.asarray(dv), np.asarray(dt))


def test_rust_matches_golden_on_holed_mesh():
    rfn = _require_rust()
    data = _fixture()
    _check_case('holed', rfn, data)
    assert int(data['holed_loops_found']) == 1
    assert int(data['holed_patch_faces']) > 0


def test_rust_matches_golden_noop_on_closed_mesh():
    rfn = _require_rust()
    data = _fixture()
    _check_case('box', rfn, data)
    assert int(data['box_loops_found']) == 0


if __name__ == '__main__':
    _require_rust()
    fns = [v for k, v in sorted(globals().items()) if k.startswith('test_')]
    for fn in fns:
        fn()
        print('ok: %s' % fn.__name__)
    print('all %d flap Rust-parity tests passed' % len(fns))
