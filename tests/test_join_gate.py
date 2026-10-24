#!/usr/bin/env python3
"""Join (#4) shape gate (path 2: ``_evaluate`` only, no method re-flagging).

Join moves whole components, so it can change the surface without being
``invents_geometry``.  ``_evaluate`` now runs the existing one-sided
input->output Hausdorff gate for Join, using GENERATIVE_MAX_HAUSDORFF_REL.
When the deviation cannot be measured the decision is unchanged but the
record carries ``hausdorff_unknown`` (no silent skip).

Usage: ~/.local/share/sutura/venv/bin/python tests/test_join_gate.py
"""
import os
import struct
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

import methods  # noqa: E402


def _cube():
    v = np.array([
        (0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0),
        (0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)], dtype=np.float32)
    t = np.array([
        (0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7),
        (0, 1, 5), (0, 5, 4), (1, 2, 6), (1, 6, 5),
        (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)], dtype=np.int32)
    return v, t


def _write_stl(path, verts, tris):
    with open(path, 'wb') as f:
        f.write(b'join-gate-test'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(tris)))
        for a, b, c in tris:
            f.write(struct.pack('<3f', 0, 0, 0))
            f.write(struct.pack('<3f', *verts[a]))
            f.write(struct.pack('<3f', *verts[b]))
            f.write(struct.pack('<3f', *verts[c]))
            f.write(struct.pack('<H', 0))


def _watertight_result():
    return {'stage1': {'two_manifold': True, 'holes_remaining': 0,
                       'non_manifold_edges_remaining': 0},
            'stage2': {'ok': True}}


def test_join_is_still_not_invents_geometry():
    # Path 2: the method flag stays False so ranking and the "back surface was
    # estimated" note are unchanged.
    assert methods.get_method(4).invents_geometry is False


def test_join_gate_rejects_large_shift(tmp):
    v, t = _cube()
    out_v = v + np.array((0.05, 0.0, 0.0), dtype=np.float32)  # ~2.9% of diag
    path = os.path.join(tmp, 'join_shift_big.stl')
    _write_stl(path, out_v, t)
    rec = methods._evaluate(_watertight_result(), path, False,
                            [('in', v, t)], methods.get_method(4))
    assert rec['watertight'] is False, rec
    assert 'shape changed' in (rec['reason'] or ''), rec
    assert rec['hausdorff_rel'] is not None and rec['hausdorff_rel'] > 0.01, rec
    assert 'hausdorff_unknown' not in rec, rec


def test_join_gate_accepts_small_shift(tmp):
    v, t = _cube()
    out_v = v + np.array((0.001, 0.0, 0.0), dtype=np.float32)  # ~0.06% of diag
    path = os.path.join(tmp, 'join_shift_small.stl')
    _write_stl(path, out_v, t)
    rec = methods._evaluate(_watertight_result(), path, False,
                            [('in', v, t)], methods.get_method(4))
    assert rec['watertight'] is True, rec
    assert rec['hausdorff_rel'] is not None and rec['hausdorff_rel'] < 0.01, rec
    assert 'hausdorff_unknown' not in rec, rec


def test_join_gate_flags_unknown_without_comparable_input(tmp):
    v, t = _cube()
    path = os.path.join(tmp, 'join_no_input.stl')
    _write_stl(path, v, t)
    rec = methods._evaluate(_watertight_result(), path, False, [],
                            methods.get_method(4))
    assert rec['watertight'] is True, rec
    assert rec['hausdorff_unknown'] is True, rec


def test_join_gate_flags_unknown_on_object_count_mismatch(tmp):
    v, t = _cube()
    path = os.path.join(tmp, 'join_one_object.stl')
    _write_stl(path, v, t)
    in_objs = [('a', v, t), ('b', v, t)]  # 2 in, 1 out -> not comparable
    rec = methods._evaluate(_watertight_result(), path, False, in_objs,
                            methods.get_method(4))
    assert rec['watertight'] is True, rec
    assert rec['hausdorff_unknown'] is True, rec


def test_non_join_method_is_unaffected(tmp):
    v, t = _cube()
    out_v = v + np.array((0.05, 0.0, 0.0), dtype=np.float32)
    path = os.path.join(tmp, 'clean_shift.stl')
    _write_stl(path, out_v, t)
    rec = methods._evaluate(_watertight_result(), path, False,
                            [('in', v, t)], methods.get_method(1))
    assert rec['watertight'] is True, rec
    assert rec['hausdorff_rel'] is None, rec
    assert 'hausdorff_unknown' not in rec, rec


def test_record_carries_hausdorff_unknown_only_when_set():
    method = methods.get_method(4)
    base = {'watertight': True, 'holes': 0, 'non_manifold': 0,
            'hausdorff_rel': None, 'geom_change_pct': None, 'reason': 'ok'}
    plain = methods._record(method, base, 'accepted')
    assert 'hausdorff_unknown' not in plain, plain
    flagged = methods._record(method, dict(base, hausdorff_unknown=True),
                              'accepted')
    assert flagged['hausdorff_unknown'] is True, flagged


def main():
    start_cwd = os.getcwd()
    with tempfile.TemporaryDirectory(prefix='sutura-join-gate-') as tmp:
        for name, fn in sorted(globals().items()):
            if name.startswith('test_') and callable(fn):
                try:
                    os.chdir(start_cwd)
                except OSError:
                    pass
                if fn.__code__.co_argcount:
                    fn(tmp)
                else:
                    fn()
                print('ok  %s' % name)
    print('join gate tests passed')


if __name__ == '__main__':
    main()
