#!/usr/bin/env python3
"""Dependency-free STL/OBJ reader regressions.

Covers the hardening of ``sutura_engine.pipeline.mesh_io``:
  1. OBJ quads/ngons are fan-triangulated (no dropped vertices);
  2. negative OBJ indices resolve relative to the current vertex count;
  3. ``f v/vt/vn`` and ``f v//vn`` tokens use only the vertex index;
  4. ASCII STL parsing is case-insensitive and whitespace tolerant, a binary
     file with a ``solid`` header is still detected as binary, and malformed
     input raises MalformedSTLError instead of returning an empty mesh;
  5. empty OBJ/STL inputs return (0, 3) arrays, and a valid empty binary STL
     (n = 0, exact size) stays empty.

Fixtures are generated in code; no machine-local files.
Usage: ~/.local/share/sutura/venv/bin/python tests/test_mesh_io.py
"""
import os
import struct
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

from sutura_engine.pipeline.mesh_io import (  # noqa: E402
    MalformedSTLError, read_mesh, read_obj, read_stl,
)


def _write(tmp, name, data):
    path = os.path.join(tmp, name)
    mode = 'wb' if isinstance(data, bytes) else 'w'
    with open(path, mode) as f:
        f.write(data)
    return path


def _binary_stl(header, triangles):
    out = bytearray(header.ljust(80, b'\0'))
    out += struct.pack('<I', len(triangles))
    for a, b, c in triangles:
        out += struct.pack('<3f', 0, 0, 0)
        out += struct.pack('<3f', *a)
        out += struct.pack('<3f', *b)
        out += struct.pack('<3f', *c)
        out += struct.pack('<H', 0)
    return bytes(out)


def test_obj_quad_and_ngon_are_fan_triangulated():
    obj = """
v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
v 2 0 0
v 2 1 0
v 1 1 0
f 1 2 3 4
f 1 2 5 6 7
"""
    with tempfile.TemporaryDirectory() as tmp:
        v, t = read_obj(_write(tmp, 'ngon.obj', obj))
    assert v.shape == (7, 3)
    assert t.shape == (5, 3)
    assert t[0].tolist() == [0, 1, 2]
    assert t[1].tolist() == [0, 2, 3]
    assert t[2].tolist() == [0, 1, 4]
    assert t[3].tolist() == [0, 4, 5]
    assert t[4].tolist() == [0, 5, 6]


def test_obj_negative_indices_wrap_to_current_vertices():
    obj = """
v 0 0 0
v 1 0 0
v 0 1 0
f -3 -2 -1
"""
    with tempfile.TemporaryDirectory() as tmp:
        v, t = read_obj(_write(tmp, 'negative.obj', obj))
    assert v.shape == (3, 3)
    assert t.tolist() == [[0, 1, 2]]


def test_obj_slash_vertex_tokens_use_vertex_index():
    obj = """
v 0 0 0
v 1 0 0
v 0 1 0
vt 0 0
vn 0 0 1
f 1/1/1 2/2/1 3/3/1
f 1//1 2//1 3//1
"""
    with tempfile.TemporaryDirectory() as tmp:
        v, t = read_obj(_write(tmp, 'slash.obj', obj))
    assert v.shape == (3, 3)
    assert t.tolist() == [[0, 1, 2], [0, 1, 2]]


def test_obj_zero_index_is_rejected():
    obj = """
v 0 0 0
v 1 0 0
v 0 1 0
f 0 1 2
"""
    with tempfile.TemporaryDirectory() as tmp:
        path = _write(tmp, 'zero.obj', obj)
        try:
            read_obj(path)
        except ValueError as exc:
            assert 'index 0' in str(exc)
        else:
            raise AssertionError('OBJ index 0 must raise ValueError')


def test_stl_ascii_is_case_insensitive_and_whitespace_tolerant():
    stl = (
        'SOLID ascii-cube\n'
        '  FACET   NORMAL 0 0 1\n'
        '\tOUTER LOOP\n'
        '      VERTEX   0.0\t0.0  0.0\n'
        '      vertex 1 0 0\n'
        '      Vertex 0 1 0\n'
        '    ENDLOOP\n'
        '  ENDFACET\n'
        'ENDSOLID\n'
    )
    with tempfile.TemporaryDirectory() as tmp:
        v, t = read_stl(_write(tmp, 'upper.stl', stl))
    assert v.shape == (3, 3)
    assert v.tolist() == [[0, 0, 0], [1, 0, 0], [0, 1, 0]]
    assert t.tolist() == [[0, 1, 2]]


def test_stl_binary_with_solid_header_is_detected():
    tris = [((0, 0, 0), (1, 0, 0), (0, 1, 0))]
    blob = _binary_stl(b'solid this is really binary', tris)
    with tempfile.TemporaryDirectory() as tmp:
        v, t = read_stl(_write(tmp, 'solid-binary.stl', blob))
    assert v.shape == (3, 3)
    assert t.tolist() == [[0, 1, 2]]
    assert v.tolist() == [[0, 0, 0], [1, 0, 0], [0, 1, 0]]


def test_stl_malformed_raises_named_error():
    # Binary-looking header whose declared count does not match the file size
    # and whose body has no ASCII vertex records.
    bad = _binary_stl(b'malformed', [((0, 0, 0), (1, 0, 0), (0, 1, 0))])
    bad = bad[:84] + struct.pack('<I', 999) + bad[84:]
    with tempfile.TemporaryDirectory() as tmp:
        path = _write(tmp, 'bad.stl', bad)
        try:
            read_stl(path)
        except MalformedSTLError as exc:
            assert 'Malformed STL' in str(exc)
        else:
            raise AssertionError('malformed STL must raise MalformedSTLError')
        assert issubclass(MalformedSTLError, ValueError)


def test_empty_obj_returns_2d_arrays():
    with tempfile.TemporaryDirectory() as tmp:
        v, t = read_obj(_write(tmp, 'empty.obj', '# nothing here\n'))
    assert v.shape == (0, 3) and v.dtype == np.float32
    assert t.shape == (0, 3) and t.dtype == np.int32


def test_empty_binary_stl_stays_valid_empty_mesh():
    blob = _binary_stl(b'empty binary', [])
    with tempfile.TemporaryDirectory() as tmp:
        v, t = read_stl(_write(tmp, 'empty.stl', blob))
    assert v.shape == (0, 3) and v.dtype == np.float32
    assert t.shape == (0, 3) and t.dtype == np.int32


def test_read_mesh_dispatches_by_extension():
    tris = [((0, 0, 0), (1, 0, 0), (0, 1, 0))]
    with tempfile.TemporaryDirectory() as tmp:
        s_path = _write(tmp, 'part.STL', _binary_stl(b'dispatch', tris))
        v, t = read_mesh(s_path)
    assert v.shape == (3, 3) and t.tolist() == [[0, 1, 2]]


def main():
    test_obj_quad_and_ngon_are_fan_triangulated()
    test_obj_negative_indices_wrap_to_current_vertices()
    test_obj_slash_vertex_tokens_use_vertex_index()
    test_obj_zero_index_is_rejected()
    test_stl_ascii_is_case_insensitive_and_whitespace_tolerant()
    test_stl_binary_with_solid_header_is_detected()
    test_stl_malformed_raises_named_error()
    test_empty_obj_returns_2d_arrays()
    test_empty_binary_stl_stays_valid_empty_mesh()
    test_read_mesh_dispatches_by_extension()
    print('mesh_io reader regressions passed')


if __name__ == '__main__':
    main()
