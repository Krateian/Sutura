#!/usr/bin/env python3
"""Parity of native one-ring face dilation with PyMeshLab's filter.

The selection is always produced by PyMeshLab's SI filter (or a synthetic
selection for the synthetic cases); only the dilation step is native.
"""
import json
import os
import subprocess
import sys

try:
    import pymeshlab
except ImportError:
    pymeshlab = None

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')

WORKER = r'''
import json, sys, os
import numpy as np
sys.path.insert(0, os.path.join(sys.argv[1], 'sutura'))
import pymeshlab as ml
import topology

def compare(ms, label):
    m = ms.current_mesh()
    v = np.asarray(m.vertex_matrix(), np.float64)
    t = np.asarray(m.face_matrix(), np.int64)
    before = np.asarray(m.face_selection_array(), dtype=bool)
    ms.apply_filter('apply_selection_dilatation')
    pml = np.asarray(ms.current_mesh().face_selection_array(), dtype=bool)
    nat = topology.dilate_face_selection(t, before)
    ok = bool(np.array_equal(pml, nat))
    return {'label': label, 'faces': int(len(t)), 'selected': int(before.sum()),
            'pml': int(pml.sum()), 'native': int(nat.sum()), 'same': ok,
            'pml_only': int((pml & ~nat).sum()), 'native_only': int((nat & ~pml).sum())}

if sys.argv[2] == 'synthetic':
    cases = []
    # cube with a hand-set selection
    h = 0.5
    v = np.array([[-h,-h,-h],[h,-h,-h],[h,h,-h],[-h,h,-h],
                  [-h,-h,h],[h,-h,h],[h,h,h],[-h,h,h]], np.float64)
    t = np.array([[0,2,1],[0,3,2],[4,5,6],[4,6,7],[0,1,5],[0,5,4],
                  [3,7,6],[3,6,2],[0,4,7],[0,7,3],[1,2,6],[1,6,5]], np.int32)
    for name, sel in [('one', [0]), ('pair', [0, 5]), ('all', list(range(12)))]:
        ms = ml.MeshSet(); ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
        mask = np.zeros(len(t), np.float64); mask[sel] = 1.0
        ms.current_mesh().add_face_custom_scalar_attribute(mask, '_sel')
        ms.apply_filter('compute_selection_by_condition_per_face', condselect='_sel > 0.5')
        cases.append(compare(ms, name))
    print(json.dumps(cases))
else:
    ms = ml.MeshSet(); ms.load_new_mesh(sys.argv[2])
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    print(json.dumps([compare(ms, os.path.basename(sys.argv[2]))]))
'''

def run_worker(arg):
    p = subprocess.run([sys.executable, '-c', WORKER, REPO, arg],
                       capture_output=True, text=True, timeout=900,
                       env=dict(os.environ, PYTHONPATH=SUTURA))
    assert p.returncode == 0, p.stderr[-2000:]
    return json.loads(p.stdout.strip().splitlines()[-1])

def main():
    if pymeshlab is None:
        print("skip: pymeshlab required for dilation parity oracle")
        return
    meshes = subprocess.check_output(
        ['git', '-C', REPO, 'ls-files', 'tests/real-world-samples/*.stl'],
        text=True).splitlines()
    assert len(meshes) == 40, len(meshes)
    rows = []
    for rel in meshes:
        rows.extend(run_worker(os.path.join(REPO, rel)))
    rows.extend(run_worker('synthetic'))
    bad = [r for r in rows if not r['same']]
    print('dilate parity: %d cases, %d mismatches' % (len(rows), len(bad)))
    if bad:
        for r in bad:
            print('MISMATCH', r)
        raise SystemExit(1)
    print('dilate parity passed')


def test_dilate_parity():
    if pymeshlab is None:
        print("skip: pymeshlab required for dilation parity oracle")
        return
    main()


if __name__ == '__main__':
    main()
