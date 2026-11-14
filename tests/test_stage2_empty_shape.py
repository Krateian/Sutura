#!/usr/bin/env python3
"""Stage 2 empty/error shape regression (P10 lead review R3).

The historical contract is (0, 3) arrays; an empty Vec must not become (0, 0).
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402


def test_empty_stage2_returns_0x3():
    import sutura_geom
    ov, ot, rep = sutura_geom.stage2_rebuild(
        np.zeros((0, 3), dtype=np.float64),
        np.zeros((0, 3), dtype=np.int32))
    assert ov.shape == (0, 3), ov.shape
    assert ot.shape == (0, 3), ot.shape
    assert rep.get('ok') is False


def test_open_mesh_error_returns_0x3():
    import sutura_geom
    verts = np.array([[0.0, 0.0, 0.0],
                      [1.0, 0.0, 0.0],
                      [0.0, 1.0, 0.0]], dtype=np.float64)
    tris = np.array([[0, 1, 2]], dtype=np.int32)
    ov, ot, rep = sutura_geom.stage2_rebuild(verts, tris)
    assert ov.shape[1] == 3, ov.shape
    assert ot.shape[1] == 3, ot.shape
    if rep.get('ok') is False:
        assert ov.shape == (0, 3)
        assert ot.shape == (0, 3)


if __name__ == '__main__':
    test_empty_stage2_returns_0x3()
    test_open_mesh_error_returns_0x3()
    print('stage2 empty shape passed')
