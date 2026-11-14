#!/usr/bin/env python3
"""OBJ writer precision regression (P10 lead review R1).

The Rust writer formats float32; float64 callers must stay on the Python
writer so the saved text keeps the caller's precision.
"""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

from sutura_engine.pipeline.mesh_io import write_obj  # noqa: E402


def test_write_obj_float64_keeps_python_precision():
    verts = np.array([
        [0.1234567890123, 1.0 / 3.0, 1e-7],
        [123456789.123, 0.1, 3.141592653589793],
    ], dtype=np.float64)
    tris = np.array([[0, 1, 2]], dtype=np.int64)
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, 'precision.obj')
        write_obj(path, verts, tris)
        with open(path) as f:
            text = f.read()
    assert text == (
        '# sutura intermediate\n'
        'v 0.123456789 0.333333333 1e-07\n'
        'v 123456789 0.1 3.14159265\n'
        'f 1 2 3\n'
    ), text


if __name__ == '__main__':
    test_write_obj_float64_keeps_python_precision()
    print('write_obj float64 precision passed')
