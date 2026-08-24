#!/usr/bin/env python3
"""Regression tests for before_after_render.py's main render flow.

Runs the real before_after_render.py as a subprocess (the same isolation the
GUI uses) on a realistic half-face-defect cube pair and a clean cube pair,
then checks the produced PNGs:

  1. a half-face defect mesh renders with > 0 defect-red pixels in
     main_before (the worst defect is now directed toward the camera),
     and all four PNGs are non-empty.
  2. a clean mesh renders with 0 red pixels everywhere (no defects,
     R=None -> fixed isometric fallback).

The half-face defect (one triangle of the target face missing) keeps the
defect rim attached to the remaining surface so the camera-facing hole is
actually visible; a fully-removed face would hide it (Adim 4 finding).

Run with any Python that has numpy + PySide6 (the sutura venv; the subprocess
needs pymeshlab too, which the venv has), with QT_QPA_PLATFORM=offscreen so
no display is needed.
Usage: QT_QPA_PLATFORM=offscreen ~/.local/share/sutura/venv/bin/python tests/test_before_after_render.py
"""
import os
import struct
import subprocess
import sys
import tempfile

os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402
from PySide6.QtGui import QImage  # noqa: E402


# --- mesh helpers -----------------------------------------------------------

def _cube_corners(c):
    x, y, z = c
    return np.array([
        [x + 1, y, z], [x + 1, y, z + 1], [x + 1, y + 1, z], [x + 1, y + 1, z + 1],
        [x, y, z], [x, y, z + 1], [x, y + 1, z], [x, y + 1, z + 1],
    ], dtype=np.float64)


def _cube_faces(idx):
    """Quad index tuples for a unit cube (order: +X, -X, -Z, +Z, -Y, +Y)."""
    return [
        (idx[0], idx[1], idx[3], idx[2]),  # +X
        (idx[4], idx[6], idx[7], idx[5]),  # -X
        (idx[0], idx[2], idx[6], idx[4]),  # -Z
        (idx[1], idx[5], idx[7], idx[3]),  # +Z
        (idx[0], idx[4], idx[5], idx[1]),  # -Y
        (idx[2], idx[3], idx[7], idx[6]),  # +Y
    ]


def _faces(quads):
    """Triangulate quad index tuples into a triangle list."""
    tris = []
    for a, b, c, d in quads:
        tris.append((a, b, c))
        tris.append((a, c, d))
    return tris


def _write_stl(path, verts, tris):
    with open(path, 'wb') as f:
        f.write(b'\x00' * 80)
        f.write(struct.pack('<I', len(tris)))
        for tri in tris:
            p0, p1, p2 = verts[list(tri)]
            n = np.cross(p1 - p0, p2 - p0)
            ln = float(np.linalg.norm(n)) or 1.0
            n = n / ln
            f.write(struct.pack('<3f', *n))
            for p in (p0, p1, p2):
                f.write(struct.pack('<3f', *p))
            f.write(struct.pack('<H', 0))


def _cube_pair(name, face_quad_index, tmpdir):
    """Write (name.stl, name_repaired.stl) into tmpdir: the defect mesh has
    ONLY the FIRST triangle of the target face missing (half the face open,
    rim stays attached), the repaired mesh is the full closed cube."""
    verts = _cube_corners((0.0, 0.0, 0.0))
    all_quads = _cube_faces(list(range(8)))
    full_tris = _faces(all_quads)
    # face quad occupies positions 2*f, 2*f+1 in full_tris
    defect_tris = [t for i, t in enumerate(full_tris) if i != 2 * face_quad_index]
    base = os.path.join(tmpdir, name)
    _write_stl(base + '.stl', verts, defect_tris)
    _write_stl(base + '_repaired.stl', verts, full_tris)


def _red_count(path):
    """Count defect-red pixels (R-G>40 and R-B>40, RGB888) in a PNG."""
    img = QImage(path)
    if img.isNull():
        return None, img.width(), img.height()
    img = img.convertToFormat(QImage.Format_RGB888)
    w, h = img.width(), img.height()
    ptr = img.constBits()
    if isinstance(ptr, memoryview):
        ptr = ptr.tobytes()
    a = np.frombuffer(ptr, dtype=np.uint8).reshape((h, w, 3)).astype(np.int16)
    red = int(((a[:, :, 0] - a[:, :, 1] > 40) & (a[:, :, 0] - a[:, :, 2] > 40)).sum())
    return red, w, h


def _render(tmpdir, defect_stl, repaired_stl):
    """Run before_after_render.py as a subprocess (same isolation as the GUI)
    writing PNGs into tmpdir; returns (paths, exit_code)."""
    renderer = os.path.join(SUTURA, 'before_after_render.py')
    paths = {k: os.path.join(tmpdir, '%s.png' % k)
             for k in ('main_before', 'main_after', 'detail_before', 'detail_after')}
    proc = subprocess.run(
        [sys.executable, renderer, defect_stl, repaired_stl,
         paths['main_before'], paths['main_after'],
         paths['detail_before'], paths['detail_after'],
         '720', '540'],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
    return paths, proc.returncode


# --- tests ------------------------------------------------------------------

def test_render_with_defect_has_red_pixels():
    with tempfile.TemporaryDirectory(prefix='sutura-ba-') as tmp:
        _cube_pair('plusx', 0, tmp)  # +X face, half open
        paths, rc = _render(tmp, os.path.join(tmp, 'plusx.stl'),
                            os.path.join(tmp, 'plusx_repaired.stl'))
        assert rc == 0, rc
        # all four PNGs exist and are non-empty
        for p in paths.values():
            assert os.path.getsize(p) > 0, p
        # the worst defect must be visible in main_before AND detail_before
        # (the detail close-up frames the same defect)
        for k in ('main_before', 'detail_before'):
            red, w, h = _red_count(paths[k])
            assert red is not None and red > 0, (k, red, w, h)
        # after views are the closed cube: no defect red anywhere
        for k in ('main_after', 'detail_after'):
            red, _, _ = _red_count(paths[k])
            assert red == 0, (k, red)


def test_render_clean_has_no_red():
    with tempfile.TemporaryDirectory(prefix='sutura-ba-') as tmp:
        # clean pair: both files are the same closed cube
        _cube_pair('clean', 0, tmp)
        closed = os.path.join(tmp, 'clean_repaired.stl')
        paths, rc = _render(tmp, closed, closed)
        assert rc == 0, rc
        for p in paths.values():
            assert os.path.getsize(p) > 0, p
        for k, p in paths.items():
            red, w, h = _red_count(p)
            assert red == 0, (k, red, w, h)


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('before/after render tests passed')


if __name__ == '__main__':
    main()