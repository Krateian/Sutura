#!/usr/bin/env python3
"""Labeled synthetic mesh generator for the mesh classifier.

Produces a deterministic set of (verts, tris, ground_truth_label) meshes:
clearly mechanical primitives (boxes, gears, lattices, extruded profiles)
and clearly organic ones (UV/ico spheres, torus, capsule, noisy blobs),
each at a few levels of detail, plus damaged variants (holes + debris).

Everything is built in memory with numpy (+ trimesh for a few primitives);
no files are written and no network is used, so tests and the calibration
harness stay fast and reproducible. Importing this module pulls in numpy and
trimesh only -- never pymeshlab.

Usage:
    from tests.make_classifier_set import iter_meshes
    for m in iter_meshes():
        m['name'], m['verts'], m['tris'], m['label']

Label values: 'mechanical' | 'organic'.
"""
import numpy as np
import trimesh

_RNG = np.random.default_rng(42)


# ---------------------------------------------------------------- helpers

def _as_arrays(mesh):
    return (np.asarray(mesh.vertices, dtype=np.float32),
            np.asarray(mesh.faces, dtype=np.int32))


def _extrude_profile(poly2d, height, z0=0.0):
    """Extrude a 2D polygon (star-shaped w.r.t. its centroid) into a closed
    prism mesh. Caps are triangulated as a fan from the centroid, so this
    works for any simple polygon without requiring shapely."""
    poly = np.asarray(poly2d, dtype=np.float64)
    center = poly.mean(axis=0)
    n = len(poly)
    bottom = np.concatenate([poly, np.full((n, 1), z0)], axis=1)
    top = np.concatenate([poly, np.full((n, 1), z0 + height)], axis=1)
    cb = np.append(center, z0)
    ct = np.append(center, z0 + height)
    verts = np.vstack([bottom, top, cb, ct])
    tris = []
    # caps: fan from the centroid vertex (indices 2n, 2n+1)
    for i in range(n):
        j = (i + 1) % n
        tris.append([2 * n, j, i])          # bottom cap (down)
        tris.append([2 * n + 1, n + i, n + j])  # top cap (up)
    # side quads
    for i in range(n):
        j = (i + 1) % n
        tris.append([i, j, n + j])
        tris.append([i, n + j, n + i])
    return np.asarray(verts, dtype=np.float32), np.asarray(tris, dtype=np.int32)


def _add_debris(verts, tris, n):
    """Append n tiny boxes around the mesh (disconnected debris)."""
    lo = verts.min(axis=0)
    hi = verts.max(axis=0)
    span = hi - lo
    out = [verts, tris]
    for _ in range(n):
        c = lo + span * (_RNG.random(3) * 0.3 + 1.3)
        s = 0.02 * span.mean()
        box_v, box_t = _as_arrays(trimesh.creation.box(extents=(s, s, s)))
        out[0] = np.vstack([out[0], box_v + c])
        out[1] = np.vstack([out[1], box_t + len(out[0]) - len(box_v)])
    return out[0], out[1]


def _remove_faces(verts, tris, frac):
    """Drop a fraction of faces at random (opens holes / cracks)."""
    drop = _RNG.choice(len(tris), size=int(len(tris) * frac), replace=False)
    keep = np.ones(len(tris), dtype=bool)
    keep[drop] = False
    return verts, tris[keep]


def _damage(verts, tris, holes=0.04, debris=2):
    """Return a damaged variant: random holes plus disconnected debris."""
    v, t = _remove_faces(verts, tris, holes)
    return _add_debris(v, t, debris)


# ----------------------------------------------------------- mechanical

def _boxes():
    out = []
    for name, ext in [
        ('box_1x1x1', (1.0, 1.0, 1.0)),
        ('box_3x1x1', (3.0, 1.0, 1.0)),
        ('box_1x1x4', (1.0, 1.0, 4.0)),
        ('box_2x05x1', (2.0, 0.5, 1.0)),
    ]:
        v, t = _as_arrays(trimesh.creation.box(extents=ext))
        out.append((name, v, t, 'mechanical'))
    return out


def _gears():
    out = []
    for teeth in (6, 12, 24):
        ang = np.linspace(0, 2 * np.pi, teeth, endpoint=False)
        pts = []
        for a in ang:
            pts.append([np.cos(a), np.sin(a)])
            pts.append([1.3 * np.cos(a + np.pi / teeth),
                        1.3 * np.sin(a + np.pi / teeth)])
            pts.append([np.cos(a + 2 * np.pi / teeth),
                        np.sin(a + 2 * np.pi / teeth)])
        v, t = _extrude_profile(np.array(pts), 0.4)
        out.append(('gear_%d' % teeth, v, t, 'mechanical'))
    return out


def _lattices():
    out = []
    for n_bars in (3, 5):
        bars = []
        for axis in (0, 1, 2):
            for k in range(n_bars):
                ext = [0.05, 0.05, 0.05]
                ext[axis] = 1.2
                b = trimesh.creation.box(extents=tuple(ext))
                off = [0.0, 0.0, 0.0]
                off[axis] = 0.0
                # place bars along each axis, spaced across the other two
                if axis == 0:
                    off[1] = (k - (n_bars - 1) / 2) * 0.3
                    off[2] = (k - (n_bars - 1) / 2) * 0.3
                elif axis == 1:
                    off[0] = (k - (n_bars - 1) / 2) * 0.3
                    off[2] = (k - (n_bars - 1) / 2) * 0.3
                else:
                    off[0] = (k - (n_bars - 1) / 2) * 0.3
                    off[1] = (k - (n_bars - 1) / 2) * 0.3
                b.apply_translation(off)
                bars.append(b)
        merged = trimesh.util.concatenate(bars)
        v, t = _as_arrays(merged)
        out.append(('lattice_%d' % n_bars, v, t, 'mechanical'))
    return out


def _extruded_profiles():
    out = []
    # L-shaped profile: clearly mechanical, non-trivial silhouette
    pts = np.array([[0, 0], [1, 0], [1, 0.35], [0.35, 0.35],
                    [0.35, 1], [0, 1]], dtype=np.float64)
    v, t = _extrude_profile(pts, 0.8)
    out.append(('extrude_L', v, t, 'mechanical'))
    # T-shaped profile
    pts = np.array([[0, 0], [1, 0], [1, 0.3], [0.65, 0.3],
                    [0.65, 1], [0.35, 1], [0.35, 0.3], [0, 0.3]],
                   dtype=np.float64)
    v, t = _extrude_profile(pts, 0.6)
    out.append(('extrude_T', v, t, 'mechanical'))
    return out


# ------------------------------------------------------------- organic

def _uv_sphere(count):
    """Clean UV sphere via trimesh (no degenerate pole triangles)."""
    m = trimesh.creation.uv_sphere(radius=1.0, count=(count, count))
    return _as_arrays(m)


def _blob(count, noise):
    """Blob-like noisy surface: UV sphere warped by low-frequency sin/cos
    noise applied to the vertex radii."""
    m = trimesh.creation.uv_sphere(radius=1.0, count=(count, count))
    verts = np.asarray(m.vertices, dtype=np.float64)
    r = np.linalg.norm(verts, axis=1)
    x, y, z = verts[:, 0], verts[:, 1], verts[:, 2]
    phi = np.arccos(np.clip(y / r, -1, 1))
    theta = np.arctan2(z, x)
    warp = (np.sin(4 * phi) * np.cos(3 * theta)
            + 0.5 * np.sin(7 * phi) * np.cos(5 * theta))
    verts = verts * (1.0 + noise * warp[:, None])
    return np.asarray(verts, dtype=np.float32), np.asarray(m.faces, dtype=np.int32)


def _organic():
    out = []
    for count in (8, 16, 32):
        out.append(('uv_sphere_%d' % count,
                    *_uv_sphere(count), 'organic'))
    for sub in (2, 3, 4):
        out.append(('ico_sphere_%d' % sub,
                    *_as_arrays(trimesh.creation.icosphere(subdivisions=sub)),
                    'organic'))
    for maj, mnr in ((32, 16), (48, 24), (64, 32)):
        m = trimesh.creation.torus(major_radius=1.0, minor_radius=0.3,
                                   major_sections=maj, minor_sections=mnr)
        out.append(('torus_%d_%d' % (maj, mnr), *_as_arrays(m), 'organic'))
    for sec in (16, 32, 64):
        m = trimesh.creation.capsule(radius=0.5, height=1.5, count=(sec, sec * 2))
        out.append(('capsule_%d' % sec, *_as_arrays(m), 'organic'))
    for noise in (0.05, 0.12, 0.2):
        out.append(('blob_%.2f' % noise, *_blob(16, noise), 'organic'))
    return out


# ------------------------------------------------------------- the set

def iter_meshes():
    """Yield dicts: {'name', 'verts', 'tris', 'label'} for every mesh."""
    mechanical = _boxes() + _gears() + _lattices() + _extruded_profiles()
    organic = _organic()

    # damaged variants: one mechanical, one organic
    _name, bv, bt, _label = _boxes()[0]
    damaged_mech = _damage(bv, bt)
    _name, sv, st, _label = _organic()[0]  # uv_sphere_16
    damaged_org = _damage(sv, st)

    rows = []
    for name, v, t, label in mechanical:
        rows.append((name, v, t, label))
    for name, v, t, label in organic:
        rows.append((name, v, t, label))
    rows.append(('damaged_box', *damaged_mech, 'mechanical'))
    rows.append(('damaged_uv_sphere', *damaged_org, 'organic'))

    for name, v, t, label in rows:
        yield {'name': name, 'verts': v, 'tris': t, 'label': label}


def main():
    n_mech = n_org = 0
    for m in iter_meshes():
        n_mech += m['label'] == 'mechanical'
        n_org += m['label'] == 'organic'
        print('%-20s %-10s verts=%-7d tris=%d' % (
            m['name'], m['label'], len(m['verts']), len(m['tris'])))
    print('total: %d mechanical, %d organic' % (n_mech, n_org))


if __name__ == '__main__':
    main()