#!/usr/bin/env python3
"""Stage 2 bridge: rebuild a closed mesh as a valid manifold3d solid.

Runs under the python3.11 virtualenv (manifold3d ships no wheel for
Python 3.14). Reads an OBJ produced by stage 1, builds a Manifold,
merges overlapping shells with a boolean union, and writes an OBJ for
the caller to re-import and save in the requested format.
"""
import sys
import json
import numpy as np
import trimesh
import manifold3d as m3d


def bbox_diag(verts):
    lo = verts.min(axis=0)
    hi = verts.max(axis=0)
    return float(np.linalg.norm(hi - lo))


def write_obj(path, verts, tris):
    with open(path, 'w') as f:
        f.write('# manifold3d repair output\n')
        for v in verts:
            f.write('v %.9g %.9g %.9g\n' % (v[0], v[1], v[2]))
        for t in tris:
            f.write('f %d %d %d\n' % (t[0] + 1, t[1] + 1, t[2] + 1))


def run_bridge(src, dst):
    """Rebuild the closed OBJ at src into a manifold solid at dst.

    Returns the JSON report dict. Used both by the CLI entry point (as a
    separate interpreter) and, when manifold3d is importable in the current
    process, by repair.py in-process.
    """
    mesh = trimesh.load(src, force='mesh')
    verts = np.asarray(mesh.vertices, dtype=np.float32)
    tris = np.asarray(mesh.faces, dtype=np.int32)

    report = {
        'input_vertices': int(len(verts)),
        'input_faces': int(len(tris)),
        'input_watertight': bool(mesh.is_watertight),
    }

    man = m3d.Manifold(m3d.Mesh(vert_properties=verts, tri_verts=tris))
    report['construct_status'] = str(man.status())

    if man.is_empty():
        report['error'] = 'manifold3d could not process the input (%s)' % man.status()
        return report

    report['volume_before'] = float(man.volume())

    parts = man.decompose()
    if len(parts) > 1:
        report['shells_merged'] = len(parts)
        man = m3d.Manifold.batch_boolean(parts, m3d.OpType.Add)
        report['volume_after_union'] = float(man.volume())

    out = man.to_mesh()
    out_verts = np.asarray(out.vert_properties)[:, :3]
    out_tris = np.asarray(out.tri_verts)

    report['output_vertices'] = int(len(out_verts))
    report['output_faces'] = int(len(out_tris))
    report['volume_after'] = float(man.volume())
    report['shells'] = len(man.decompose())

    write_obj(dst, out_verts, out_tris)
    report['ok'] = True
    return report


def main():
    src, dst = sys.argv[1], sys.argv[2]
    report = run_bridge(src, dst)
    print(json.dumps(report))


if __name__ == '__main__':
    main()