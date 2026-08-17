#!/usr/bin/env python3
"""Generate a synthetic layered multi-object 3MF for regression testing.

Reproduces a real Bambu Studio export failure mode: the archive has more
than one object mesh, and every vertex position is duplicated ~15x as
separate entries, each layer with its own triangle list. A pipeline that
round-trips through STL and re-deduplicates by position corrupts this into
a triangle soup (edges shared by many faces); Sutura must preserve the
original vertex structure and output valid 2-manifold meshes.

Usage:
    make_layered_multiobject_3mf.py [OUTPUT]          # write the 3MF
    make_layered_multiobject_3mf.py --check [OUTPUT]  # also repair + validate

--check fails (exit 1) if an object is lost, the output is not a valid zip,
or an object's output has edges shared by more than 2 faces.
"""
import os
import re
import sys
import subprocess
import zipfile

LAYERS = 15
OUTPUT = 'test-layered-multiobject.3mf'

# ---------------------------------------------------------------- geometry

# a cube with the top face removed (an open hole) and one inverted face
CUBE_V = [
    (-1, -1, -1), (1, -1, -1), (1, 1, -1), (-1, 1, -1),
    (-1, -1, 1), (1, -1, 1), (1, 1, 1), (-1, 1, 1),
]
CUBE_T = [
    (0, 2, 1), (0, 3, 2),          # bottom
    (0, 1, 5), (0, 5, 4),          # back
    (1, 2, 6), (1, 6, 5),          # right
    (2, 3, 7), (2, 7, 6),          # front
    (3, 0, 4), (3, 4, 7),          # left
]
CUBE_T[4] = (CUBE_T[4][2], CUBE_T[4][1], CUBE_T[4][0])  # inverted winding

# a fold: two extra triangles on the bottom-back edge make that edge
# shared by 4 faces (a non-manifold fold, like the real-world exports)
CUBE_V = CUBE_V + [(0.0, -1.3, -1.0), (0.0, -1.3, 0.0)]
CUBE_T = CUBE_T + [(0, 1, 8), (0, 1, 9)]

# a healthy cube for the second object (12 faces, kept by the repair)
CUBE_FULL_V = CUBE_V
CUBE_FULL_T = [
    (0, 2, 1), (0, 3, 2),          # bottom
    (4, 5, 6), (4, 6, 7),          # top
    (0, 1, 5), (0, 5, 4),          # back
    (1, 2, 6), (1, 6, 5),          # right
    (2, 3, 7), (2, 7, 6),          # front
    (3, 0, 4), (3, 4, 7),          # left
]


def layer_mesh(verts, tris):
    """Duplicate a mesh N times as overlapping vertex layers."""
    v = verts * LAYERS
    t = []
    for layer in range(LAYERS):
        off = layer * len(verts)
        t += [(a + off, b + off, c + off) for a, b, c in tris]
    return v, t


# ------------------------------------------------------------- 3MF writing

CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
 <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
 <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
</Types>
"""

RELS = """<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
 <Relationship Target="/3D/3dmodel.model" Id="rel-1" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>
"""

MODEL_RELS = """<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
 <Relationship Target="/3D/Objects/object_1.model" Id="rel-1" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
 <Relationship Target="/3D/Objects/object_2.model" Id="rel-2" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>
"""

MAIN_MODEL = """<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xml:lang="en-US" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <resources>
  <object id="2" type="model">
   <components>
    <component p:path="/3D/Objects/object_1.model" objectid="1" transform="1 0 0 0 1 0 0 0 1 0 0 0"/>
   </components>
  </object>
  <object id="4" type="model">
   <components>
    <component p:path="/3D/Objects/object_2.model" objectid="3" transform="1 0 0 0 1 0 0 0 1 20 0 0"/>
   </components>
  </object>
 </resources>
 <build>
  <item objectid="2" transform="1 0 0 0 1 0 0 0 1 0 0 0" printable="1"/>
  <item objectid="4" transform="1 0 0 0 1 0 0 0 1 0 0 0" printable="1"/>
 </build>
</model>
"""


def object_model(obj_id, verts, tris):
    lines = ['<?xml version="1.0" encoding="UTF-8"?>',
             '<model unit="millimeter" xml:lang="en-US" '
             'xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">',
             ' <resources>',
             '  <object id="%d" type="model">' % obj_id,
             '   <mesh>',
             '    <vertices>']
    for v in verts:
        lines.append('     <vertex x="%.7g" y="%.7g" z="%.7g"/>' % (v[0], v[1], v[2]))
    lines.append('    </vertices>')
    lines.append('    <triangles>')
    for t in tris:
        lines.append('     <triangle v1="%d" v2="%d" v3="%d"/>' % (t[0], t[1], t[2]))
    lines.append('    </triangles>')
    lines.append('   </mesh>')
    lines.append('  </object>')
    lines.append(' </resources>')
    lines.append('</model>')
    return '\n'.join(lines) + '\n'


def write_3mf(path, obj1_v, obj1_t, obj2_v, obj2_t):
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
        z.writestr('[Content_Types].xml', CONTENT_TYPES)
        z.writestr('_rels/.rels', RELS)
        z.writestr('3D/_rels/3dmodel.model.rels', MODEL_RELS)
        z.writestr('3D/3dmodel.model', MAIN_MODEL)
        z.writestr('3D/Objects/object_1.model', object_model(1, obj1_v, obj1_t))
        z.writestr('3D/Objects/object_2.model', object_model(3, obj2_v, obj2_t))


# ---------------------------------------------------------------- checking

def edge_multiplicity(path, model_name):
    """Return the number of edges shared by 3+ faces in the given model."""
    with zipfile.ZipFile(path) as z:
        xml = z.read(model_name).decode('utf-8', errors='replace')
    tris = [(int(a), int(b), int(c)) for a, b, c in re.findall(
        r'<triangle\s+v1="(\d+)"\s+v2="(\d+)"\s+v3="(\d+)"[^>]*/>', xml)]
    edges = {}
    for a, b, c in tris:
        for e in ((a, b), (b, c), (c, a)):
            e = tuple(sorted(e))
            edges[e] = edges.get(e, 0) + 1
    return sum(1 for n in edges.values() if n >= 3)


def check(output):
    """Repair the generated 3MF with sutura and validate the result."""
    sutura = os.path.expanduser('~/.local/bin/sutura')
    fixed = output[:-4] + '_fixed' + output[-4:]
    r = subprocess.run([sutura, output], capture_output=True, text=True, timeout=600)
    try:
        import json
        rep = json.loads(r.stdout.strip().splitlines()[-1])
    except Exception:
        print('FAIL: sutura output not parseable\n%s' % r.stdout[-500:])
        return 1

    fails = []
    objects = rep.get('object_reports', [])
    if len(objects) != 2:
        fails.append('expected 2 objects, got %d' % len(objects))

    if not zipfile.ZipFile(fixed).testzip() is None:
        fails.append('output 3MF is not a valid zip')

    for name in ('3D/Objects/object_1.model', '3D/Objects/object_2.model'):
        if name not in zipfile.ZipFile(fixed).namelist():
            fails.append('output missing %s' % name)
            continue
        nm = edge_multiplicity(fixed, name)
        if nm > 0:
            fails.append('%s has %d edges shared by 3+ faces (soup corruption)' % (name, nm))

    for i, obj in enumerate(objects):
        s1 = obj.get('stage1', {})
        if not s1.get('two_manifold'):
            fails.append('object %d not two-manifold' % i)

    if fails:
        print('FAIL:')
        for f in fails:
            print('  - %s' % f)
        return 1
    print('PASS: 2 objects, both two-manifold, no soup corruption')
    for i, obj in enumerate(objects):
        s1 = obj.get('stage1', {})
        print('  object %d: %d hole(s) remaining' % (i, s1.get('holes_remaining', 0)))
    return 0


def main():
    argv = sys.argv[1:]
    check_mode = '--check' in argv
    out = next((a for a in argv if not a.startswith('--')), OUTPUT)
    if not out.endswith('.3mf'):
        out += '.3mf'

    write_3mf(out,
              *layer_mesh(CUBE_V, CUBE_T),
              *layer_mesh(CUBE_FULL_V, CUBE_FULL_T))
    print('wrote %s (2 objects, %d vertex layers each)' % (out, LAYERS))

    if check_mode:
        return check(out)
    return 0


if __name__ == '__main__':
    sys.exit(main())