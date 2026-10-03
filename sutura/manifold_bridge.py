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


# Stage-1 hole filling can leave flat, effectively zero-volume "debris" shells.
# manifold3d ingests them as degenerate 2D sheets, but its boolean union cuts
# through them and plants thousands of residual self-intersections that the
# rest of the pipeline then has to carry. They carry no printable volume
# (effective thickness 2*V/A below DEBRIS_FLATNESS of their own bounding-box
# diagonal, or an absolute volume below DEBRIS_VOLUME_EPS cubic units) so
# dropping them before the union removes only debris. Both tests are
# conservative: a genuine plate or thin feature is orders of magnitude
# thicker. Measured on the ornate-frame Stage-2 input: 13 parts dropped
# (effective thickness 4e-9..2.5e-6 units), volume change 3e-8 relative,
# self-intersections 6,441 -> 4,704.
DEBRIS_VOLUME_EPS = 1e-3
DEBRIS_FLATNESS = 1e-4


def _is_debris_part(part):
    """True when a decomposed part is a flat/collapsed shell, not a solid."""
    try:
        vol = abs(float(part.volume()))
        if vol <= 0.0:
            return True
        if vol <= DEBRIS_VOLUME_EPS:
            return True
        area = float(part.surface_area())
        bb = part.bounding_box()
        diag = float(np.linalg.norm([bb[3] - bb[0], bb[4] - bb[1], bb[5] - bb[2]]))
        if area <= 0.0 or diag <= 0.0:
            return True
        return (2.0 * vol / area) <= DEBRIS_FLATNESS * diag
    except Exception:  # noqa: BLE001 - an unreadable part is kept, never dropped
        return False


def _drop_post_union_debris(man, report):
    """Drop micro-slivers the boolean union isolated from the input shells.

    The pre-union filter removes flat/zero-volume shells, but the CSG union
    itself can clip intersecting shells at shallow angles and leave tiny
    disconnected fragments that no slicer should print. Decompose the unioned
    solid, keep the largest part unconditionally and drop only parts that
    satisfy ``_is_debris_part``; the surviving parts are already disjoint, so
    they are reassembled with ``compose`` (no further CSG cuts, which could
    create new slivers). Records ``report['post_union_debris_dropped']`` when
    anything is dropped and returns ``(manifold, dropped)``.
    """
    parts = man.decompose()
    if len(parts) <= 1:
        return man, 0
    vols = [abs(float(p.volume())) for p in parts]
    largest_idx = int(np.argmax(vols))
    kept = [p for i, p in enumerate(parts)
            if i == largest_idx or not _is_debris_part(p)]
    dropped = len(parts) - len(kept)
    if not dropped:
        return man, 0
    # kept is never empty: the largest part is always retained.
    if len(kept) == 1:
        man = kept[0]
    else:
        man = m3d.Manifold.compose(kept)
    report['post_union_debris_dropped'] = int(dropped)
    return man, dropped


def write_obj(path, verts, tris):
    with open(path, 'w') as f:
        f.write('# manifold3d repair output\n')
        for v in verts:
            f.write('v %.9g %.9g %.9g\n' % (v[0], v[1], v[2]))
        for t in tris:
            f.write('f %d %d %d\n' % (t[0] + 1, t[1] + 1, t[2] + 1))


def run_bridge(src, dst):
    """Rebuild the closed OBJ at src into a manifold solid at dst; returns
    the report dict. Shared by the CLI entry point and repair.py in-process."""
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

    # Pre-union debris filter: drop collapsed/flat shells before boolean union.
    parts = man.decompose()
    report['shells_found'] = len(parts)
    if len(parts) > 1:
        kept = [p for p in parts if not _is_debris_part(p)]
        dropped = len(parts) - len(kept)
        if dropped:
            report['debris_parts_dropped'] = int(dropped)
        if not kept:  # never drop everything; fall back to the construct
            kept = parts
        report['shells_merged'] = len(kept)
        if len(kept) == 1:
            man = kept[0]
        else:
            man = m3d.Manifold.batch_boolean(kept, m3d.OpType.Add)
        report['volume_after_union'] = float(man.volume())

    # Post-union debris filter: CSG can cut or isolate micro-slivers.
    man, _ = _drop_post_union_debris(man, report)

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


def watertight_check(verts, tris):
    """Independent manifold3d watertight/manifold verdict for a triangle mesh.

    Cross-validation layer (FAZ10) that sits NEXT TO the pymeshlab-based
    defects.detect() strict check (holes==0 AND non_manifold==0); it never
    replaces it. A mesh is watertight iff a Manifold can be constructed with
    Error.NoError AND the result is non-empty (an open or non-manifold mesh
    produces Error.NotManifold and an empty Manifold).

    Returns ``(ok, status)`` where ``ok`` is True/False/None (None when
    manifold3d is unavailable in this environment) and ``status`` is the
    str(Error) or the reason. Never raises for bad input.
    """
    try:
        import numpy as np
        import manifold3d as m3d
        v = np.asarray(verts, dtype=np.float32)
        t = np.asarray(tris, dtype=np.int32)
        if len(v) == 0 or len(t) == 0:
            return False, 'empty'
        man = m3d.Manifold(m3d.Mesh(vert_properties=v, tri_verts=t))
        status = man.status()
        ok = bool(status == m3d.Error.NoError) and not man.is_empty()
        return ok, str(status)
    except ImportError:
        return None, 'manifold3d unavailable'
    except Exception as e:  # noqa: BLE001 - a bad mesh must never crash the check
        return False, '%s: %s' % (type(e).__name__, e)


def main():
    src, dst = sys.argv[1], sys.argv[2]
    report = run_bridge(src, dst)
    print(json.dumps(report))


if __name__ == '__main__':
    main()