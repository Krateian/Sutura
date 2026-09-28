#!/usr/bin/env python3
"""Regression test for outer-shell extraction pre-step in the auto repair path.

Checks:
  1. should_extract_outer_shell returns True for overlapping/nested closed
     components in mechanical/dense-SI models (e.g. 3x3 cube assemblies).
  2. should_extract_outer_shell returns False for single-component meshes
     and spatially separated components.
  3. repair_mesh_from_arrays in auto mode extracts the outer shell, removes
     degenerate internal geometry, and repairs to a strict-watertight solid.
  4. Runtime target: < 5 s.

Usage: ~/.local/share/sutura/venv/bin/python tests/test_outer_shell_auto.py
"""
import os
import sys
import tempfile
import time

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import repair  # noqa: E402

try:
    import trimesh
    import manifold3d
except ImportError as e:
    print('skip tests/test_outer_shell_auto.py: %s' % e)
    sys.exit(0)


def test_should_extract_outer_shell():
    # 1. Single cube -> False
    c = trimesh.creation.box(extents=(1, 1, 1))
    assert not repair.should_extract_outer_shell(c.vertices, c.faces, 'mechanical')

    # 2. Two spatially separated cubes -> False
    c1 = trimesh.creation.box(extents=(1, 1, 1)).apply_translation([0, 0, 0])
    c2 = trimesh.creation.box(extents=(1, 1, 1)).apply_translation([10, 0, 0])
    sep = trimesh.util.concatenate([c1, c2])
    assert not repair.should_extract_outer_shell(sep.vertices, sep.faces, 'mechanical')

    # 3. Two touching/overlapping cubes -> True
    c3 = trimesh.creation.box(extents=(1, 1, 1)).apply_translation([0.9, 0, 0])
    overlap = trimesh.util.concatenate([c1, c3])
    assert repair.should_extract_outer_shell(overlap.vertices, overlap.faces, 'mechanical')

    # 4. Organic mesh without heavy SI -> False
    assert not repair.should_extract_outer_shell(overlap.vertices, overlap.faces, 'organic', si_count=0)


def test_outer_shell_repair_3x3_assembly():
    # Synthetic 3x3 cube assembly (27 touching cubies, Rubik-style)
    cubes = [
        trimesh.creation.box(extents=(1.0, 1.0, 1.0)).apply_translation([x, y, z])
        for x in [-1.0, 0.0, 1.0]
        for y in [-1.0, 0.0, 1.0]
        for z in [-1.0, 0.0, 1.0]
    ]
    assembly = trimesh.util.concatenate(cubes)
    v = np.asarray(assembly.vertices, np.float32)
    t = np.asarray(assembly.faces, np.int32)

    with tempfile.TemporaryDirectory() as td:
        t0 = time.perf_counter()
        rep, out_v, out_t = repair.repair_mesh_from_arrays(v, t, td, mode='auto')
        out_v, out_t = repair.maybe_run_stage2(rep, out_v, out_t, td)
        repair.enforce_reload_verdict(rep, out_v, out_t)
        elapsed = time.perf_counter() - t0

        assert rep.get('outer_shell_extracted') is True, rep
        s1 = rep.get('stage1', {})
        assert s1.get('two_manifold') is True, s1
        assert s1.get('holes_remaining', 0) == 0, s1
        assert s1.get('components', 0) == 1, s1
        s2 = rep.get('stage2', {})
        assert s2.get('ok') is True, s2
        assert elapsed < 5.0, 'Outer shell repair took too long: %.2f s' % elapsed

    print('test_outer_shell_repair_3x3_assembly passed (%.2f s)' % elapsed)


if __name__ == '__main__':
    test_should_extract_outer_shell()
    test_outer_shell_repair_3x3_assembly()
    print('all outer shell auto tests passed')
