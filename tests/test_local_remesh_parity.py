#!/usr/bin/env python3
"""Parity test for the local-remesh deep-repair tier.

The production Python tier ``_local_remesh_tier`` and the PyO3 bridge
``sutura_core_py.local_remesh_tier`` (which calls
``sutura_core::local_remesh::local_remesh``) must return the same arrays and
the same topological report on a mesh the tier actually adopts.

The bridge is only present after a rebuild of ``sutura_core_py``; until then
the test skips cleanly instead of failing.  The two public corpus meshes below
are known to produce an adopted candidate, so a passing run exercises the real
remesh path, not just a skip.

Runnable directly (needs numpy, trimesh and the built extension):
    <venv>/bin/python tests/test_local_remesh_parity.py
"""
import json
import os
import sys

import pytest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for p in (SUTURA, REPO):
    if p not in sys.path:
        sys.path.insert(0, p)

np = pytest.importorskip('numpy')
trimesh = pytest.importorskip('trimesh')
topology = pytest.importorskip('topology')

from sutura_engine.pipeline.tiers.local_remesh import _local_remesh_tier  # noqa: E402

# Two public meshes whose Python tier adopts a candidate: a small Thingi10K
# scan and a large Artec scan.  Both are tracked under tests/real-world-samples.
MESHES = (
    'thingi10k_100173.stl',
    'artec_bovine-heart.stl',
)

BOOL_FIELDS = ('ran', 'adopted', 'outside_unchanged')
INT_FIELDS = (
    'faces_removed',
    'faces_added',
    'regions',
    'holes_before',
    'holes_after',
    'nm_before',
    'nm_after',
    'max_loop_len',
    'work_units',
    'work_budget',
    'fairing_iters',
    'si_before',
    'si_after',
)


def _bridge():
    try:
        import sutura_core_py
    except Exception as exc:  # noqa: BLE001 - a missing extension is a skip
        pytest.skip('sutura_core_py is not importable: %s' % exc)
    fn = getattr(sutura_core_py, 'local_remesh_tier', None)
    if fn is None:
        pytest.skip(
            'sutura_core_py.local_remesh_tier is missing (rebuild the extension)'
        )
    return fn


def _load(name):
    path = os.path.join(REPO, 'tests', 'real-world-samples', name)
    if not os.path.exists(path):
        pytest.skip('missing public sample %s' % name)
    mesh = trimesh.load(path, process=True)
    return (
        np.asarray(mesh.vertices, dtype=np.float64),
        np.asarray(mesh.faces, dtype=np.int64),
    )


def _python_tier(v, t):
    after = topology.mesh_topology(v, t)
    py_v, py_t, _after, rep = _local_remesh_tier(v, t, v, t, after)
    return py_v, py_t, rep


def _report(rep):
    out = {key: bool(rep.get(key)) for key in BOOL_FIELDS}
    out.update({key: rep.get(key) for key in INT_FIELDS})
    return out


@pytest.mark.parametrize('name', MESHES)
def test_python_tier_matches_rust_bridge(name):
    fn = _bridge()
    v, t = _load(name)
    py_v, py_t, py_rep = _python_tier(v, t)
    assert py_rep.get('adopted') is True, py_rep

    rs_v, rs_t, rs_json = fn(v, t)
    rs_rep = json.loads(rs_json)

    assert np.array_equal(np.asarray(py_v), np.asarray(rs_v)), 'vertices differ'
    assert np.array_equal(np.asarray(py_t), np.asarray(rs_t)), 'triangles differ'
    assert _report(py_rep) == _report(rs_rep), 'report fields differ'


@pytest.mark.parametrize('name', MESHES)
def test_rust_bridge_is_deterministic(name):
    fn = _bridge()
    v, t = _load(name)
    a_v, a_t, a_json = fn(v, t)
    b_v, b_t, b_json = fn(v, t)
    assert np.array_equal(np.asarray(a_v), np.asarray(b_v)), 'vertices differ'
    assert np.array_equal(np.asarray(a_t), np.asarray(b_t)), 'triangles differ'
    assert _report(json.loads(a_json)) == _report(json.loads(b_json))


if __name__ == '__main__':
    raise SystemExit(pytest.main([__file__, '-q']))
