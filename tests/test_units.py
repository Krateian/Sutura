#!/usr/bin/env python3
"""Unit-detection heuristic tests for sutura/repair.py.

Checks that:
  1. check_units() flags meshes whose bounding box is only plausible when
     scaled from inches or centimetres to millimetres (a model authored in
     the wrong unit), while a normal mm-scale mesh stays silent.
  2. Empty / zero-extent meshes never warn (no size judgment possible).
  3. The 3MF <model unit="..."> attribute is definitive: declared inch ->
     warning, declared millimeter (the spec default) -> trusted, heuristic
     suppressed.
  4. read_3mf_units() parses the unit attribute from every .model file and
     defaults to millimeter when absent.
  5. The warning reaches the CLI JSON + --human output for both a real
     repair and validate.

Uses the repo's own repair.py under the sutura venv (numpy + pymeshlab).
Usage: ~/.local/share/sutura/venv/bin/python tests/test_units.py
       (macOS: /opt/homebrew/.../envs/sutura-env/bin/python tests/test_units.py)
"""
import json
import os
import subprocess
import sys
import tempfile
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402
import repair  # noqa: E402

REPAIR_PY = os.path.join(SUTURA, 'repair.py')


def _run(args):
    r = subprocess.run([sys.executable, REPAIR_PY] + args,
                       capture_output=True, text=True, timeout=600)
    return r


def _json(r):
    return json.loads(r.stdout.strip().splitlines()[-1])


def _hint(verts, declared_unit=None):
    r = repair.check_units(np.asarray(verts, dtype=np.float32), declared_unit)
    return r


# --- check_units unit tests ------------------------------------------------

def test_inch_scale_cube_warns():
    # a 6-unit cube: as mm it is tiny, as inches ~152 mm it is plausible
    r = _hint([[0, 0, 0], [6, 0, 0]])
    assert r is not None, 'a 6-unit cube should warn'
    assert r['unit_warning'] is True
    assert 'inches' in r['unit_hint'] or 'not in millimetres' in r['unit_hint']
    assert '6' in r['unit_hint']


def test_cm_scale_only_warns():
    # a 3-unit cube: inches -> 76 mm, cm -> 30 mm; either scale is plausible
    r = _hint([[0, 0, 0], [3, 0, 0]])
    assert r is not None


def test_one_unit_min_edge_warns():
    # 1 unit -> 25.4 mm as inches (the bottom edge of the plausible range)
    r = _hint([[0, 0, 0], [1, 0, 0]])
    assert r is not None


def test_sub_mm_cube_silent():
    # 0.9 unit -> 22.9 mm as inches, below the plausible range
    assert _hint([[0, 0, 0], [0.9, 0, 0]]) is None


def test_mm_scale_cube_silent():
    # 60 / 300 / 500 units are all plausible millimetre sizes
    for size in (60, 300, 500):
        assert _hint([[0, 0, 0], [size, 0, 0]]) is None, size


def test_inch_cube_equiv_warns():
    # exactly 10 inches as units (254 mm): still flagged as inches
    r = _hint([[0, 0, 0], [10, 0, 0]])
    assert r is not None


def test_declared_inch_is_definitive():
    r = _hint([[0, 0, 0], [60, 0, 0]], declared_unit='inch')
    assert r is not None
    assert 'inch' in r['unit_hint']


def test_declared_millimeter_is_trusted():
    # 6 units but the file says millimeter: no heuristic warning
    assert _hint([[0, 0, 0], [6, 0, 0]], declared_unit='millimeter') is None


def test_empty_and_degenerate_silent():
    assert repair.check_units(np.empty((0, 3))) is None
    assert repair.check_units(None) is None
    assert repair.check_units(np.array([[0, 0, 0], [0, 0, 0]])) is None


# --- read_3mf_units unit tests ---------------------------------------------

_MODEL = ('<?xml version="1.0" encoding="UTF-8"?>'
          '<model unit="%s" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">'
          '<resources><object id="1" type="model"><mesh><vertices>'
          '<vertex x="0" y="0" z="0"/><vertex x="6" y="0" z="0"/>'
          '<vertex x="0" y="6" z="0"/><vertex x="0" y="0" z="6"/>'
          '</vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles>'
          '</mesh></object></resources></model>')


def _write_3mf(path, unit_attr):
    if unit_attr is not None:
        xml = _MODEL % unit_attr
    else:
        # no unit attribute at all -> the 3MF spec default (millimeter)
        xml = (_MODEL % 'millimeter').replace(' unit="millimeter"', '')
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
        z.writestr('3D/3dmodel.model', xml)


def test_read_3mf_units(tmp):
    path = os.path.join(tmp, 'u.3mf')
    _write_3mf(path, 'inch')
    assert repair.read_3mf_units(path) == {'3D/3dmodel.model': 'inch'}
    _write_3mf(path, 'millimeter')
    assert repair.read_3mf_units(path) == {'3D/3dmodel.model': 'millimeter'}


def test_read_3mf_units_defaults_to_millimeter(tmp):
    path = os.path.join(tmp, 'u.3mf')
    _write_3mf(path, None)
    assert repair.read_3mf_units(path) == {'3D/3dmodel.model': 'millimeter'}


# --- CLI-level tests -------------------------------------------------------

def _make_cube_stl(path, size, broken=True):
    import trimesh
    m = trimesh.creation.box(extents=[size, size, size])
    if broken:
        m.update_faces(np.arange(len(m.faces) - 1))
    with open(path, 'wb') as f:
        f.write(b'sutura-units'.ljust(80, b'\0'))
        f.write(len(m.faces).to_bytes(4, 'little'))
        for face in m.faces:
            f.write(b'\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00')
            for idx in face:
                f.write(np.asarray(m.vertices[idx], dtype=np.float32).tobytes())
            f.write(b'\x00\x00')
    assert os.path.getsize(path) == 84 + len(m.faces) * 50


def test_repair_reports_unit_warning(tmp):
    path = os.path.join(tmp, 'small.stl')
    _make_cube_stl(path, size=6)
    r = _run([path])
    assert r.returncode == 0, r.stderr
    d = _json(r)
    assert d.get('unit_warning') is True, d
    assert d.get('unit_hint'), d
    hr = _run([path, '--human'])
    assert 'WARNING:' in hr.stdout, hr.stdout


def test_repair_mm_scale_no_warning(tmp):
    path = os.path.join(tmp, 'normal.stl')
    _make_cube_stl(path, size=60)
    d = _json(_run([path]))
    assert d.get('unit_warning') is None, d


def test_validate_reports_unit_warning(tmp):
    path = os.path.join(tmp, 'small.stl')
    _make_cube_stl(path, size=6)
    r = _run(['validate', path])
    assert r.returncode == 0, r.stderr
    v = _json(r).get('validation', {})
    assert v.get('unit_warning') is True, v
    hr = _run(['validate', path, '--human'])
    assert 'WARNING:' in hr.stdout, hr.stdout


def test_validate_3mf_declared_inch(tmp):
    path = os.path.join(tmp, 'inch.3mf')
    _write_3mf(path, 'inch')
    r = _run(['validate', path])
    assert r.returncode == 0, r.stderr
    v = _json(r).get('validation', {})
    assert v.get('unit_warning') is True, v
    assert 'inch' in (v.get('unit_hint') or ''), v


def test_validate_3mf_declared_mm_silent(tmp):
    path = os.path.join(tmp, 'mm.3mf')
    _write_3mf(path, 'millimeter')
    r = _run(['validate', path])
    assert r.returncode == 0, r.stderr
    v = _json(r).get('validation', {})
    assert v.get('unit_warning') is None, v


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-units-') as tmp:
        for name, fn in sorted(globals().items()):
            if name.startswith('test_') and callable(fn):
                try:
                    if fn.__code__.co_argcount == 1:
                        fn(tmp)
                    else:
                        fn()
                    print('ok  %s' % name)
                except AssertionError as e:
                    print('FAIL %s: %s' % (name, e))
                    return 1
    print('unit tests passed')
    return 0


if __name__ == '__main__':
    sys.exit(main())
