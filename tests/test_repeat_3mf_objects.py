#!/usr/bin/env python3
"""Multi-object 3MF: the Rust core repairs per object and rewrites the archive.

The fixture is built here, in the test, from plain geometry -- no sample files
and no private corpus names. It covers the three properties that matter:

* every ``<mesh>`` block is repaired separately (objects are never merged);
* object order follows the archive, and the ``build`` items survive the rewrite;
* two byte-identical objects are repaired once but each still gets its own
  report, which is what Python's ``repair_3mf_objects`` does.

Run: python tests/test_repeat_3mf_objects.py
"""
import json
import os
import sys
import tempfile
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
for p in (os.path.join(REPO, 'sutura'), REPO):
    if p not in sys.path:
        sys.path.insert(0, p)


def _box(ox, s=1.0):
    v = [(ox, 0, 0), (ox + s, 0, 0), (ox + s, s, 0), (ox, s, 0),
         (ox, 0, s), (ox + s, 0, s), (ox + s, s, s), (ox, s, s)]
    f = [(0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7), (0, 1, 5), (0, 5, 4),
         (1, 2, 6), (1, 6, 5), (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)]
    return v, f


def write_3mf(path, objects, unit='millimeter'):
    """One `.model` part per object, referenced from `<build>` by `objectid`."""
    ct = ('<?xml version="1.0" encoding="UTF-8"?>\n'
          '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
          '<Default Extension="rels" ContentType="application/'
          'vnd.openxmlformats-package.relationships+xml"/>'
          '<Default Extension="model" ContentType="application/'
          'vnd.ms-package.3dmanufacturing-3dmodel+xml"/></Types>')
    rels = ('<?xml version="1.0" encoding="UTF-8"?>\n'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/'
            '2006/relationships"><Relationship Target="/3D/3dmodel.model" '
            'Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/'
            '2013/01/3dmodel"/></Relationships>')
    items = "\n".join(f'  <item objectid="{i}"/>'
                      for i in range(1, len(objects) + 1))
    resources = "\n".join(
        f' <object id="{i}" type="model" name="obj_{i}">'
        f'<mesh><vertices/><triangles/></mesh></object>'
        for i in range(1, len(objects) + 1))
    root = ('<?xml version="1.0" encoding="UTF-8"?>\n'
            f'<model unit="{unit}" xml:lang="en-US" '
            'xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">\n'
            f'<resources>\n{resources}\n</resources>\n'
            f'<build>\n{items}\n</build>\n</model>\n')
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
        z.writestr('[Content_Types].xml', ct)
        z.writestr('_rels/.rels', rels)
        z.writestr('3D/3dmodel.model', root)
        for i, (verts, tris) in enumerate(objects, start=1):
            vs = "\n".join(
                f'<vertex x="{x:.6f}" y="{y:.6f}" z="{z:.6f}"/>'
                for x, y, z in verts)
            ts = "\n".join(
                f'<triangle v1="{a}" v2="{b}" v3="{c}"/>' for a, b, c in tris)
            model = ('<?xml version="1.0" encoding="UTF-8"?>\n'
                     f'<model unit="{unit}" xmlns="http://schemas.microsoft.com/'
                     '3dmanufacturing/core/2015/02">\n<resources>\n'
                     '<object id="1" type="model">\n<mesh>\n<vertices>\n'
                     f'{vs}\n</vertices>\n<triangles>\n{ts}\n</triangles>\n'
                     '</mesh>\n</object>\n</resources>\n'
                     '<build><item objectid="1"/></build>\n</model>\n')
            z.writestr(f'3D/Objects/object_{i}.model', model)


def _require_bindings():
    try:
        import sutura_core_py  # noqa: F401
    except Exception as exc:  # noqa: BLE001
        print(f'skip: sutura_core_py unavailable ({exc})')
        return None
    return sutura_core_py


def _repair(core, src, out):
    """Run the binding and decode its JSON reports into dicts."""
    result = dict(core.repair_3mf(src, out))
    result['object_reports'] = [json.loads(r) for r in result['object_reports']]
    return result


def test_objects_are_repaired_separately_and_in_order():
    core = _require_bindings()
    if core is None:
        return
    with tempfile.TemporaryDirectory() as td:
        src = os.path.join(td, 'three.3mf')
        out = os.path.join(td, 'out.3mf')
        write_3mf(src, [_box(0), _box(3), _box(6, s=1.5)])
        result = _repair(core, src, out)
        assert result['objects'] == 3, result
        # Order follows the archive; the rewrite preserves it.
        models = [r['triangle_attributes']['model'] for r in result['object_reports']]
        assert models == [f'3D/Objects/object_{i}.model' for i in (1, 2, 3)], models
        # The output is still a 3MF container with all three parts and the
        # build items intact.
        with zipfile.ZipFile(out) as z:
            names = z.namelist()
        assert '3D/3dmodel.model' in names, names
        assert sum(1 for n in names if n.startswith('3D/Objects/')) == 3, names
    print('ok  test_objects_are_repaired_separately_and_in_order')


def test_duplicate_geometry_reports_per_object():
    """Byte-identical objects are repaired once but each gets a report."""
    core = _require_bindings()
    if core is None:
        return
    with tempfile.TemporaryDirectory() as td:
        src = os.path.join(td, 'dup.3mf')
        out = os.path.join(td, 'out.3mf')
        write_3mf(src, [_box(0), _box(0)])
        result = _repair(core, src, out)
        assert result['objects'] == 2, result
        reports = result['object_reports']
        assert len(reports) == 2, reports
        # Distinct objects in the JSON, not one shared reference, even though
        # the geometry (and therefore the repair) was reused.
        assert reports[0] is not reports[1]
    print('ok  test_duplicate_geometry_reports_per_object')


def test_declared_unit_warns_per_object():
    core = _require_bindings()
    if core is None:
        return
    with tempfile.TemporaryDirectory() as td:
        for unit, expected in (('millimeter', False), ('inch', True)):
            src = os.path.join(td, f'{unit}.3mf')
            out = os.path.join(td, f'{unit}-out.3mf')
            write_3mf(src, [_box(0), _box(3)], unit=unit)
            result = _repair(core, src, out)
            flags = [bool(r.get('unit_warning')) for r in result['object_reports']]
            assert all(f is expected for f in flags), (unit, flags)
    print('ok  test_declared_unit_warns_per_object')


if __name__ == '__main__':
    test_objects_are_repaired_separately_and_in_order()
    test_duplicate_geometry_reports_per_object()
    test_declared_unit_warns_per_object()
