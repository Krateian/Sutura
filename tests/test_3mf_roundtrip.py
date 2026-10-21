#!/usr/bin/env python3
"""Non-destructive 3MF round-trip regressions.

Checks that a repair rewrites only the repaired mesh XML:
  - every non-mesh entry keeps its bytes, name, order and ZIP metadata;
  - Production-extension manifests (3D/Objects/*.model) stay intact;
  - multiple meshes in one .model entry survive (span-drift regression);
  - a single-object Production file is loadable without PyMeshLab;
  - phase-1 triangle attributes survive only on exact-coordinate triangles.

Fixtures are generated in a temporary directory; no machine-local files.
Usage: ~/.local/share/sutura/venv/bin/python tests/test_3mf_roundtrip.py
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
REPAIR_PY = os.path.join(SUTURA, 'repair.py')
sys.path.insert(0, SUTURA)

import repair  # noqa: E402
import sutura_engine.triage as engine_triage  # noqa: E402

CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
 <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
 <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
 <Default Extension="png" ContentType="image/png"/>
</Types>
"""

RELS = """<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
 <Relationship Target="/3D/3dmodel.model" Id="rel-1" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>
"""

MODEL_RELS = """<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
 <Relationship Target="/3D/Objects/object_1.model" Id="rel-2" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>
"""

CUBE_V = [
    (-1, -1, -1), (1, -1, -1), (1, 1, -1), (-1, 1, -1),
    (-1, -1, 1), (1, -1, 1), (1, 1, 1), (-1, 1, 1),
]
CUBE_T = [
    (0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7), (0, 1, 5), (0, 5, 4),
    (1, 2, 6), (1, 6, 5), (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7),
]
OPEN_T = CUBE_T[:2] + CUBE_T[4:]


def _mesh(v, t, attrs=None):
    vs = ''.join('<vertex x="%g" y="%g" z="%g"/>' % p for p in v)
    ts = []
    for i, tri in enumerate(t):
        extra = attrs[i] if attrs and i < len(attrs) and attrs[i] else ''
        ts.append('<triangle v1="%d" v2="%d" v3="%d"%s/>'
                  % (tri[0], tri[1], tri[2], extra))
    return '<mesh><vertices>%s</vertices><triangles>%s</triangles></mesh>' % (
        vs, ''.join(ts))


def _model(objects, build_items, extra_ns=''):
    return ('<?xml version="1.0" encoding="UTF-8"?>'
            '<model unit="millimeter" xml:lang="en-US" '
            'xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"%s>'
            '<resources>%s</resources><build>%s</build></model>'
            % (extra_ns, objects, build_items))


def _object(oid, mesh, extra=''):
    return ('<object id="%d" type="model"%s>%s</object>'
            % (oid, extra, mesh))


def _write_zip(path, entries, comment=b'sutura-test-archive'):
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
        z.comment = comment
        for name, data, meta in entries:
            info = zipfile.ZipInfo(name, date_time=meta.get(
                'date_time', (2020, 1, 2, 3, 4, 6)))
            info.compress_type = meta.get('compress_type',
                                          zipfile.ZIP_DEFLATED)
            info.external_attr = meta.get('external_attr', 0o644 << 16)
            info.comment = meta.get('comment', b'entry-%s' % name.encode())
            info.extra = meta.get('extra', b'\x00\x00')
            z.writestr(info, data)


def _entry(name, data, **meta):
    if isinstance(data, str):
        data = data.encode('utf-8')
    return (name, data, meta)


def _run(src, out, tmp):
    env = dict(os.environ)
    env['XDG_CACHE_HOME'] = os.path.join(tmp, 'cache')
    env['SUTURA_DATA_DIR'] = os.path.join(tmp, 'data')
    os.makedirs(env['XDG_CACHE_HOME'], exist_ok=True)
    os.makedirs(env['SUTURA_DATA_DIR'], exist_ok=True)
    r = subprocess.run(
        [sys.executable, REPAIR_PY, src, '-o', out, '--mode', 'low',
         '--no-history', '--json'],
        capture_output=True, text=True, timeout=600, env=env)
    assert r.returncode == 0, (r.stdout, r.stderr)
    return json.loads(r.stdout.strip().splitlines()[-1])


def _is_model_entry(name):
    return name.lower().endswith('.model')


def _outside_meshes(path):
    """Model XML with every <mesh>...</mesh> span removed."""
    with zipfile.ZipFile(path) as z:
        xml = {n: z.read(n).decode('utf-8', errors='replace')
               for n in z.namelist() if _is_model_entry(n)}
    spans = {}
    for b in repair.parse_3mf_mesh_blocks(path):
        spans.setdefault(b.model_name, []).append(b.span)
    out = {}
    for name, text in xml.items():
        parts = []
        pos = 0
        for start, end in sorted(spans.get(name, [])):
            parts.append(text[pos:start])
            pos = end
        parts.append(text[pos:])
        out[name] = ''.join(parts)
    return out


def _assert_preserved(src, out):
    with zipfile.ZipFile(src) as a, zipfile.ZipFile(out) as b:
        assert a.namelist() == b.namelist(), (a.namelist(), b.namelist())
        assert a.comment == b.comment, (a.comment, b.comment)
        for name in a.namelist():
            ia, ib = a.getinfo(name), b.getinfo(name)
            assert ia.date_time == ib.date_time, name
            assert ia.compress_type == ib.compress_type, name
            assert ia.external_attr == ib.external_attr, name
            assert ia.comment == ib.comment, name
            assert ia.extra == ib.extra, name
            if not _is_model_entry(name):
                assert a.read(name) == b.read(name), name
    assert _outside_meshes(src) == _outside_meshes(out)


def test_single_object_preserves_project_metadata(tmp):
    src = os.path.join(tmp, 'single_meta.3mf')
    out = os.path.join(tmp, 'single_meta_fixed.3mf')
    attrs = [' paint_color="1"' if i % 2 else '' for i in range(len(OPEN_T))]
    model = _model(
        _object(7, _mesh(CUBE_V, OPEN_T, attrs)),
        '<item objectid="7" transform="1 0 0 0 1 0 0 0 1 0 0 0"/>',
        ' xmlns:slic3rpe="http://schemas.3mf.io/3dmanufacturing/slic3rpe"')
    _write_zip(src, [
        _entry('[Content_Types].xml', CONTENT_TYPES),
        _entry('Metadata/thumbnail.png', b'\x89PNG\r\n\x1a\n' + bytes(range(256)),
               compress_type=zipfile.ZIP_STORED),
        _entry('_rels/.rels', RELS),
        _entry('3D/3dmodel.model', model),
        _entry('Metadata/Prusa_Slicer_wipe_tower_information.xml', '<xml/>',
               external_attr=0),
        _entry('Metadata/Slic3r_PE.config', 'speed=42\n'),
        _entry('Metadata/Slic3r_PE_model.config', 'model=1\n'),
    ])
    rep = _run(src, out, tmp)
    _assert_preserved(src, out)
    assert rep['category'] != 'error', rep
    ta = rep['triangle_attributes']
    assert ta['original_attributed'] > 0, ta
    assert ta['kept'] > 0, ta
    assert ta['kept'] + ta['dropped'] == ta['original_attributed'], ta
    assert ta['output_triangles'] >= ta['kept'], ta


def test_multi_object_production_preserves_manifest(tmp):
    src = os.path.join(tmp, 'multi.3mf')
    out = os.path.join(tmp, 'multi_fixed.3mf')
    sub = _model(_object(1, _mesh(CUBE_V, OPEN_T)),
                 '<item objectid="1"/>')
    sub2 = _model(_object(3, _mesh(CUBE_V, CUBE_T)), '<item objectid="3"/>')
    sub3 = _model(_object(5, _mesh(CUBE_V, OPEN_T)), '<item objectid="5"/>')
    root = _model(
        ''.join(
            '<object id="%d" type="model"><components>'
            '<component p:path="/3D/Objects/object_%d.model" objectid="%d" '
            'transform="1 0 0 0 1 0 0 0 1 0 0 0"/></components></object>'
            % (oid + 1, i, oid) for i, oid in ((1, 1), (2, 3), (3, 5))),
        ''.join('<item objectid="%d" transform="1 0 0 0 1 0 0 0 1 %d 0 0"/>'
                % (oid + 1, i * 20) for i, oid in ((1, 1), (2, 3), (3, 5))),
        ' xmlns:p="http://schemas.microsoft.com/3dmanufacturing/production/2015/06"')
    _write_zip(src, [
        _entry('[Content_Types].xml', CONTENT_TYPES),
        _entry('_rels/.rels', RELS),
        _entry('3D/_rels/3dmodel.model.rels', MODEL_RELS),
        _entry('Metadata/project_settings.config', '{"layer_height":0.2}\n'),
        _entry('Metadata/model_settings.config', '<config/>\n'),
        _entry('Metadata/plate_1.png', b'\x89PNG\r\n\x1a\nplate'),
        _entry('3D/3dmodel.model', root),
        _entry('3D/Objects/object_1.model', sub),
        _entry('3D/Objects/object_2.model', sub2),
        _entry('3D/Objects/object_3.model', sub3),
    ])
    rep = _run(src, out, tmp)
    _assert_preserved(src, out)
    assert rep['objects'] == 3, rep
    assert rep['category'] != 'error', rep
    with zipfile.ZipFile(src) as a, zipfile.ZipFile(out) as b:
        assert a.read('3D/3dmodel.model') == b.read('3D/3dmodel.model')
    ta = rep['triangle_attributes']
    assert ta['kept'] + ta['dropped'] == ta['original_attributed'] == 0, ta


def test_two_meshes_in_one_model_entry(tmp):
    src = os.path.join(tmp, 'two_mesh_one_file.3mf')
    out = os.path.join(tmp, 'two_mesh_one_file_fixed.3mf')
    model = _model(_object(1, _mesh(CUBE_V, OPEN_T)) +
                   _object(2, _mesh(CUBE_V, OPEN_T)),
                   '<item objectid="1"/><item objectid="2"/>')
    _write_zip(src, [
        _entry('[Content_Types].xml', CONTENT_TYPES),
        _entry('_rels/.rels', RELS),
        _entry('Metadata/Slic3r_PE.config', 'x=1\n'),
        _entry('3D/3dmodel.model', model),
    ])
    rep = _run(src, out, tmp)
    _assert_preserved(src, out)
    assert rep['objects'] == 2, rep
    with zipfile.ZipFile(out) as z:
        xml = z.read('3D/3dmodel.model').decode('utf-8')
        assert xml.count('<mesh>') == 2, xml[:200]


def test_span_drift_two_model_entries(tmp):
    src = os.path.join(tmp, 'span_bug.3mf')
    out = os.path.join(tmp, 'span_bug_fixed.3mf')
    first = _model(_object(1, _mesh(CUBE_V, OPEN_T)) +
                   _object(2, _mesh(CUBE_V, OPEN_T)),
                   '<item objectid="1"/><item objectid="2"/>')
    second = _model(_object(3, _mesh(CUBE_V, OPEN_T)), '<item objectid="3"/>')
    _write_zip(src, [
        _entry('[Content_Types].xml', CONTENT_TYPES),
        _entry('_rels/.rels', RELS),
        _entry('3D/3dmodel.model', first),
        _entry('3D/Objects/o2.model', second),
    ])
    rep = _run(src, out, tmp)
    _assert_preserved(src, out)
    assert rep['objects'] == 3, rep
    with zipfile.ZipFile(out) as z:
        assert z.read('3D/3dmodel.model').decode('utf-8').count('<mesh>') == 2
        assert z.read('3D/Objects/o2.model').decode('utf-8').count('<mesh>') == 1
        import xml.etree.ElementTree as ET
        ET.fromstring(z.read('3D/3dmodel.model').decode('utf-8'))


def test_single_object_production_extension(tmp):
    src = os.path.join(tmp, 'prod_single.3mf')
    out = os.path.join(tmp, 'prod_single_fixed.3mf')
    root = _model(
        '<object id="2" type="model"><components>'
        '<component p:path="/3D/Objects/object_1.model" objectid="1" '
        'transform="1 0 0 0 1 0 0 0 1 0 0 0"/></components></object>',
        '<item objectid="2" transform="1 0 0 0 1 0 0 0 1 0 0 0" printable="1"/>',
        ' xmlns:p="http://schemas.microsoft.com/3dmanufacturing/production/2015/06"')
    sub = _model(_object(1, _mesh(CUBE_V, OPEN_T)), '<item objectid="1"/>')
    _write_zip(src, [
        _entry('[Content_Types].xml', CONTENT_TYPES),
        _entry('_rels/.rels', RELS),
        _entry('3D/_rels/3dmodel.model.rels', MODEL_RELS),
        _entry('3D/3dmodel.model', root),
        _entry('3D/Objects/object_1.model', sub),
    ])
    rep = _run(src, out, tmp)
    _assert_preserved(src, out)
    assert rep['category'] != 'error', rep
    assert rep['triangle_attributes']['model'] == '3D/Objects/object_1.model'


def test_attribute_matching_unit(tmp):
    src = os.path.join(tmp, 'attrs.3mf')
    out = os.path.join(tmp, 'attrs_fixed.3mf')
    v = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
    t = [(0, 1, 2), (0, 2, 3)]
    attrs = [' paint_color="1"', ' paint_seam="2"']
    model = _model(_object(1, _mesh(v, t, attrs)), '<item objectid="1"/>')
    _write_zip(src, [
        _entry('[Content_Types].xml', CONTENT_TYPES),
        _entry('_rels/.rels', RELS),
        _entry('Metadata/Slic3r_PE.config', 'x=1\n'),
        _entry('3D/3dmodel.model', model),
    ])
    new_v = v + [(2, 0, 0)]
    new_t = [(0, 1, 2), (4, 0, 1)]
    stats = repair.rewrite_3mf_meshes(
        src, out, [(new_v, new_t)], attr_source=src)
    ta = stats[0]
    assert ta['original_attributed'] == 2, ta
    assert ta['kept'] == 1, ta
    assert ta['dropped'] == 1, ta
    assert ta['output_triangles'] == 2, ta
    blocks = repair.parse_3mf_mesh_blocks(out)
    assert len(blocks) == 1
    out_attrs = blocks[0].triangle_attrs
    assert out_attrs[0] == ' paint_color="1"', out_attrs
    assert out_attrs[1] is None, out_attrs


def test_escalation_writer_preserves_metadata(tmp):
    src = os.path.join(tmp, 'escalation.3mf')
    out = os.path.join(tmp, 'escalation_fixed.3mf')
    model = _model(_object(1, _mesh(CUBE_V, OPEN_T)), '<item objectid="1"/>')
    _write_zip(src, [
        _entry('[Content_Types].xml', CONTENT_TYPES),
        _entry('_rels/.rels', RELS),
        _entry('Metadata/Slic3r_PE.config', 'x=1\n'),
        _entry('3D/3dmodel.model', model),
    ])
    shutil.copyfile(src, out)
    blocks = repair.parse_3mf_mesh_blocks(src)
    stats = engine_triage._replace_3mf_meshes(
        src, out, [(b.verts, b.tris) for b in blocks], attr_source=src)
    assert len(stats) == 1, stats
    _assert_preserved(src, out)


def test_case_insensitive_model_parts(tmp):
    src = os.path.join(tmp, 'case_parts.3mf')
    out = os.path.join(tmp, 'case_parts_fixed.3mf')
    m1 = _model(_object(1, _mesh(CUBE_V, OPEN_T)), '<item objectid="1"/>')
    m2 = _model(_object(3, _mesh(CUBE_V, OPEN_T)), '<item objectid="3"/>')
    _write_zip(src, [
        _entry('[Content_Types].xml', CONTENT_TYPES),
        _entry('_rels/.rels', RELS),
        _entry('3D/3dmodel.model', m1),
        _entry('3D/Objects/o2.MODEL', m2),
    ])
    assert len(repair.parse_3mf_mesh_blocks(src)) == 2
    rep = _run(src, out, tmp)
    _assert_preserved(src, out)
    assert rep['objects'] == 2, rep
    assert rep['category'] != 'error', rep

    src2 = os.path.join(tmp, 'upper.3mf')
    out2 = os.path.join(tmp, 'upper_fixed.3mf')
    _write_zip(src2, [
        _entry('[Content_Types].xml', CONTENT_TYPES),
        _entry('_rels/.rels', RELS),
        _entry('3D/3DMODEL.MODEL',
               _model(_object(1, _mesh(CUBE_V, OPEN_T)), '<item objectid="1"/>')),
    ])
    assert len(repair.parse_3mf_mesh_blocks(src2)) == 1
    rep2 = _run(src2, out2, tmp)
    assert rep2['category'] != 'error', rep2


def test_non_ascii_attribute_names_preserved(tmp):
    src = os.path.join(tmp, 'nonascii_attrs.3mf')
    out = os.path.join(tmp, 'nonascii_attrs_fixed.3mf')
    v = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
    t = [(0, 1, 2), (0, 2, 3)]
    attrs = [' \u00f6zel_renk="1"', ' \u989c\u8272="2"']
    model = _model(_object(1, _mesh(v, t, attrs)), '<item objectid="1"/>')
    _write_zip(src, [
        _entry('[Content_Types].xml', CONTENT_TYPES),
        _entry('_rels/.rels', RELS),
        _entry('3D/3dmodel.model', model),
    ])
    stats = repair.rewrite_3mf_meshes(src, out, [(v, t)], attr_source=src)
    assert stats[0]['original_attributed'] == 2, stats
    assert stats[0]['kept'] == 2, stats
    blocks = repair.parse_3mf_mesh_blocks(out)
    out_attrs = blocks[0].triangle_attrs
    assert out_attrs[0] == ' \u00f6zel_renk="1"', out_attrs
    assert out_attrs[1] == ' \u989c\u8272="2"', out_attrs


def test_escalation_refreshes_root_triangle_attributes(tmp):
    import numpy as np
    import methods
    src = os.path.join(tmp, 'escalation_attrs.3mf')
    out = os.path.join(tmp, 'escalation_attrs_fixed.3mf')
    nx = ny = 78
    xs = np.linspace(-1.0, 1.0, nx)
    ys = np.linspace(-1.0, 1.0, ny)
    X, Y = np.meshgrid(xs, ys)
    Z = 0.2 * np.cos(np.pi * X / 2.0) * np.cos(np.pi * Y / 2.0)
    v1 = [tuple(float(c) for c in row)
          for row in np.column_stack([X.ravel(), Y.ravel(), Z.ravel()])]
    t1 = []
    for i in range(ny - 1):
        for j in range(nx - 1):
            a = i * nx + j
            b = i * nx + (j + 1)
            c = (i + 1) * nx + j
            d = (i + 1) * nx + (j + 1)
            t1.append((a, b, c))
            t1.append((b, d, c))
    o0 = _object(1, _mesh(CUBE_V, CUBE_T, [' paint_color="7"'] * len(CUBE_T)))
    o1 = _object(3, _mesh(v1, t1, [' paint_color="7"'] * len(t1)))
    _write_zip(src, [
        _entry('[Content_Types].xml', CONTENT_TYPES),
        _entry('_rels/.rels', RELS),
        _entry('3D/Objects/o1.model', _model(o0, '<item objectid="1"/>')),
        _entry('3D/Objects/o2.model', _model(o1, '<item objectid="3"/>')),
    ])

    # The mesh-aware maxholesize raise lets the real baseline close the plate
    # in one pass, so no in-repo fixture escalates any more. Drive the
    # escalation path deterministically with a monkeypatched baseline/ranking
    # so the root triangle-attribute refresh after the per-object rewrite is
    # still exercised.
    from sutura_engine.methods.protocol import RepairMethod
    real_attempt = getattr(methods, '_attempt', None)
    real_evaluate = getattr(methods, '_evaluate', None)
    real_rank = getattr(methods, 'rank_methods', None)
    cube = np.asarray(CUBE_V, dtype=float)
    cube_t = np.asarray(CUBE_T, dtype=np.int64)

    def fake_attempt(src_, out_, tmpdir, ctx, multi):
        _write_zip(out_, [
            _entry('[Content_Types].xml', CONTENT_TYPES),
            _entry('_rels/.rels', RELS),
            _entry('3D/Objects/o1.model', _model(
                _object(1, _mesh(CUBE_V, CUBE_T)), '<item objectid="1"/>')),
            _entry('3D/Objects/o2.model', _model(
                _object(3, _mesh(v1, t1)), '<item objectid="3"/>')),
        ])
        return {'object_reports': [
            {'stage1': {'two_manifold': True, 'holes_remaining': 0,
                        'non_manifold_edges_remaining': 0},
             'stage2': {'ok': True}},
            {'stage1': {'two_manifold': True, 'holes_remaining': 2,
                        'non_manifold_edges_remaining': 0},
             'stage2': {'ok': False}},
        ], 'baseline': True}

    def fake_evaluate(result, path, multi, in_objs, method):
        ok = bool(isinstance(result, dict) and result.get('simulated'))
        return {'watertight': ok, 'holes': 0 if ok else 2, 'non_manifold': 0,
                'hausdorff_rel': None, 'geom_change_pct': None, 'reason': 'x'}

    def fake_rank(analysis, top_n=6):
        return [methods.Recommendation(99, 'fake_escalation', 'Fake Escalation',
                                       0.9, 'x', 'mechanical')]

    class _FakeEscalation(RepairMethod):
        def run(self, verts, tris, tmpdir, ctx=None):
            return {'_verts': cube, '_tris': cube_t, 'simulated': True,
                    'stage1': {'two_manifold': True, 'holes_remaining': 0,
                               'non_manifold_edges_remaining': 0},
                    'stage2': {'ok': True}}

    methods._attempt = fake_attempt
    methods._evaluate = fake_evaluate
    methods.rank_methods = fake_rank
    methods.register_method(_FakeEscalation(
        num=99, id='fake_escalation', name='Fake Escalation',
        description='deterministic escalation stand-in', family='test'))
    try:
        res = methods.repair_with_methods(
            src, out, tmp, None, mode='auto', deep_repair='full',
            ftetwild='auto')
    finally:
        methods.unregister_method(99)
        if real_attempt is None:
            del methods._attempt
        else:
            methods._attempt = real_attempt
        if real_evaluate is None:
            del methods._evaluate
        else:
            methods._evaluate = real_evaluate
        if real_rank is not None:
            methods.rank_methods = real_rank
    reps = res.get('object_reports', [])
    assert len(reps) == 2, reps
    sources = [r.get('method_used', {}).get('source') for r in reps]
    assert 'auto_escalated' in sources, sources
    root = res.get('triangle_attributes')
    agg = engine_triage._repair_mod().aggregate_triangle_attributes(reps)
    assert root == agg, (root, agg)
    for key in ('original_attributed', 'kept', 'dropped', 'output_triangles'):
        assert root[key] == sum(
            r['triangle_attributes'][key] for r in reps), key


def test_read_3mf_units_quotes_and_prefix(tmp):
    src = os.path.join(tmp, 'units_single.3mf')
    xml = ('<?xml version="1.0"?>'
           "<model unit='inch' "
           'xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">'
           '<resources/><build/></model>')
    _write_zip(src, [_entry('3D/3dmodel.model', xml)])
    assert repair.read_3mf_units(src) == {'3D/3dmodel.model': 'inch'}

    src2 = os.path.join(tmp, 'units_prefix.3mf')
    xml2 = ('<?xml version="1.0"?>'
            '<m:model unit="micron" '
            'xmlns:m="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">'
            '<m:resources/><m:build/></m:model>')
    _write_zip(src2, [_entry('3D/3dmodel.model', xml2)])
    assert repair.read_3mf_units(src2) == {'3D/3dmodel.model': 'micron'}


def test_zero_mesh_3mf_clear_error(tmp):
    src = os.path.join(tmp, 'empty_mesh.3mf')
    out = os.path.join(tmp, 'empty_mesh_fixed.3mf')
    xml = ('<?xml version="1.0"?>'
           '<model unit="millimeter" '
           'xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">'
           '<resources/><build/></model>')
    _write_zip(src, [_entry('3D/3dmodel.model', xml)])
    env = dict(os.environ)
    env['XDG_CACHE_HOME'] = os.path.join(tmp, 'cache')
    env['SUTURA_DATA_DIR'] = os.path.join(tmp, 'data')
    os.makedirs(env['XDG_CACHE_HOME'], exist_ok=True)
    os.makedirs(env['SUTURA_DATA_DIR'], exist_ok=True)
    r = subprocess.run(
        [sys.executable, REPAIR_PY, src, '-o', out, '--no-history', '--json'],
        capture_output=True, text=True, timeout=600, env=env)
    assert r.returncode != 0, (r.returncode, r.stdout, r.stderr)
    blob = r.stdout + r.stderr
    assert 'no mesh objects found' in blob, blob
    assert 'use repair_3mf' not in blob, blob


def test_comment_containing_mesh_tag(tmp):
    src = os.path.join(tmp, 'comment.3mf')
    out = os.path.join(tmp, 'comment_fixed.3mf')
    vertices = ''.join('<vertex x="%g" y="%g" z="%g"/>' % p for p in CUBE_V)
    triangles = ''.join('<triangle v1="%d" v2="%d" v3="%d"/>' % t
                        for t in OPEN_T)
    mesh = ('<mesh><!-- old scan: <mesh></mesh> --><vertices>%s</vertices>'
            '<triangles>%s</triangles></mesh>' % (vertices, triangles))
    model = _model(_object(1, mesh), '<item objectid="1"/>')
    _write_zip(src, [
        _entry('[Content_Types].xml', CONTENT_TYPES),
        _entry('_rels/.rels', RELS),
        _entry('Metadata/Slic3r_PE.config', 'x=1\n'),
        _entry('3D/3dmodel.model', model),
    ])
    blocks = repair.parse_3mf_mesh_blocks(src)
    assert len(blocks) == 1, blocks
    assert len(blocks[0].verts) == 8, blocks[0].verts
    rep = _run(src, out, tmp)
    assert rep['category'] != 'error', rep
    with zipfile.ZipFile(out) as z:
        import xml.etree.ElementTree as ET
        text = z.read('3D/3dmodel.model').decode('utf-8')
        assert text.count('<mesh>') == 1, text[:200]
        ET.fromstring(text)


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-3mf-roundtrip-') as tmp:
        failed = 0
        for name, fn in sorted(globals().items()):
            if name.startswith('test_') and callable(fn):
                try:
                    fn(tmp)
                    print('ok  %s' % name)
                except AssertionError as e:
                    failed += 1
                    print('FAIL %s: %s' % (name, e))
        if failed:
            return 1
    print('3mf roundtrip tests passed')
    return 0


if __name__ == '__main__':
    sys.exit(main())
