#!/usr/bin/env python3
"""Parity: the opt-in edge-tiebreak head in Python v2 and in Rust.

Compares ``mesh_classifier_v2.classify_mesh(extra_features=True)`` with the
Rust ``sutura_core_py.classify_mesh(..., extra_features=True)`` on public
sample meshes.  Skips when the Rust core wheel is not installed.

Usage: python3 tests/test_classifier_edge_extra_parity.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for p in (SUTURA, REPO):
    if p not in sys.path:
        sys.path.insert(0, p)

import numpy as np  # noqa: E402

SAMPLES = os.path.join(REPO, 'tests', 'real-world-samples')
MESHES = ('artec_bovine-heart.stl', 'artec_copper-key.stl',
          'artec_metal-nut.stl', 'thingi10k_46012.stl')


def _load(path):
    from sutura_engine.pipeline.mesh_io import load_mesh
    v, t = load_mesh(path)
    return (np.asarray(v, dtype=np.float32).astype(np.float64),
            np.asarray(t, dtype=np.int32).astype(np.int64))


def main():
    try:
        import sutura_core_py
    except ImportError:
        print('SKIP: sutura_core_py is not installed')
        return 0
    import mesh_classifier_v2 as v2

    checked = 0
    for name in MESHES:
        path = os.path.join(SAMPLES, name)
        if not os.path.exists(path):
            continue
        v, t = _load(path)
        py = v2.classify_mesh(v, t, extra_features=True)
        rs = sutura_core_py.classify_mesh(
            np.ascontiguousarray(v), np.ascontiguousarray(t),
            'experimental', True)
        assert (py['type'], py['confidence']) == (rs['type'], rs['confidence']), (
            name, py['type'], py['confidence'], rs['type'], rs['confidence'])
        for key in ('edge_oppmax_std', 'edge_don_mean', 'edge_dihed_mean',
                    'edge_rmin_mean', 'edge_gc_mean'):
            assert py['metrics'][key] == rs['metrics'][key], (
                name, key, py['metrics'][key], rs['metrics'][key])
        checked += 1
    print('OK: %d meshes match' % checked)
    return 0


if __name__ == '__main__':
    sys.exit(main())
