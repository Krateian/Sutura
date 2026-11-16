#!/usr/bin/env python3
"""Emit the numeric I/O summary used by io_parity (Python side)."""
import argparse
import hashlib
import json
import os

import numpy as np

from sutura_engine.pipeline import mesh_io


def array_sha256(verts, tris):
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    h = hashlib.sha256()
    h.update(v.astype('<f4').tobytes())
    h.update(t.astype('<i4').tobytes())
    return h.hexdigest()


def bbox(verts):
    v = np.asarray(verts)
    if len(v) == 0:
        return None
    return [float(x) for x in np.concatenate((v.min(axis=0), v.max(axis=0)))]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('path')
    ap.add_argument('--write-stl')
    a = ap.parse_args()
    ext = os.path.splitext(a.path)[1].lower()
    meshes = []
    warnings = []
    if ext == '.3mf':
        units = mesh_io.read_3mf_units(a.path)
        for block in mesh_io.parse_3mf_mesh_blocks(a.path):
            unit = units.get(block.model_name)
            w = mesh_io.check_units(block.verts, unit)
            if w is not None:
                warnings.append({'model': block.model_name, 'block': block.index,
                                 'unit_warning': w['unit_warning'],
                                 'unit_hint': w['unit_hint']})
            meshes.append({'name': block.model_name, 'index': block.index,
                           'verts': len(block.verts), 'tris': len(block.tris),
                           'bbox': bbox(block.verts),
                           'array_sha256': array_sha256(block.verts, block.tris)})
    else:
        v, t = mesh_io.load_mesh(a.path)
        w = mesh_io.check_units(v, None)
        if w is not None:
            warnings.append({'model': None, 'block': None,
                             'unit_warning': w['unit_warning'],
                             'unit_hint': w['unit_hint']})
        meshes.append({'name': None, 'index': None, 'verts': len(v),
                       'tris': len(t), 'bbox': bbox(v),
                       'array_sha256': array_sha256(v, t)})
    write_sha = None
    if a.write_stl:
        v, t = mesh_io.load_mesh(a.path)
        mesh_io.stl_write_binary(a.write_stl, v, t)
        with open(a.write_stl, 'rb') as f:
            write_sha = hashlib.sha256(f.read()).hexdigest()
    print(json.dumps({
        'format': ext.lstrip('.'),
        'bytes_in': os.path.getsize(a.path),
        'meshes': meshes,
        'unit_warnings': warnings,
        'material_refs': bool(ext == '.obj' and mesh_io.obj_has_material_refs(a.path)),
        'scan': mesh_io.scan_bad_coordinates(a.path),
        'write_sha256': write_sha,
    }, sort_keys=True))


if __name__ == '__main__':
    main()
