#!/usr/bin/env python3
"""Regression tests for the Graft (#13) tier budget and input face cap.

The Graft tier runs an unbounded r-ladder of ``morph_close`` attempts on the
original input; on dense scans that never reaches watertight it used to run
past the harness timeout.  ``graft_tier`` now honours the intensity spec's
``graft_max_faces`` (hard input cap) and ``graft_timeout`` (ladder budget) and
reports the outcome honestly instead of hanging.

Runs under the venv (numpy + the maturin-built sutura_geom extension; no
pymeshlab needed for these paths).  pytest-compatible; runnable directly.

    <venv>/bin/python tests/test_graft_budget.py
"""
import dataclasses
import os
import sys

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__))), "sutura"))

import triage  # noqa: E402
import repair  # noqa: E402
import sutura_engine.graft as graft_mod  # noqa: E402


class _FakeML:
    """Stands in for a pymeshlab module; the cap/budget paths never touch it."""
    MeshSet = object
    Mesh = object


def test_intensity_spec_graft_defaults_and_presets():
    b = triage.PRESETS['balanced']
    assert b.graft_max_faces == 2_000_000
    assert b.graft_timeout == 300.0
    assert triage.PRESETS['quick'].graft_max_faces == 2_000_000
    assert triage.PRESETS['thorough'].graft_timeout == 900.0
    assert triage.PRESETS['extreme'].graft_max_faces is None
    assert triage.PRESETS['extreme'].graft_timeout == 1800.0
    # The module constants are the Balanced defaults (as for fTetWild).
    assert repair.GRAFT_MAX_FACES == b.graft_max_faces
    assert repair.GRAFT_TIMEOUT == b.graft_timeout


def test_graft_tier_skips_over_face_cap():
    spec = dataclasses.replace(triage.PRESETS['balanced'], graft_max_faces=10)
    t = np.zeros((20, 3), np.int64)
    stats = {'stage1': {'holes_remaining': 4,
                        'non_manifold_edges_remaining': 2}}
    after = {'faces_number': 0}
    repair.graft_tier(_FakeML(), after, after, stats,
                      np.zeros((0, 3), np.float64), t, '/tmp',
                      graft=True, intensity='balanced', spec=spec)
    rec = stats['graft']
    assert rec['ran'] is False
    assert rec['adopted'] is False
    assert rec['skipped_budget'] is True
    assert rec['timed_out'] is False
    assert rec['reject_reason'] == 'too_large'
    assert rec['input_faces'] == 20
    assert rec['max_faces'] == 10
    assert rec['holes'] == 4 and rec['non_manifold'] == 2


def test_graft_tier_under_cap_runs_shell_wrap_with_budget():
    seen = {}

    def fake_shell_wrap(verts, tris, **kwargs):
        seen.update(kwargs)
        return (verts, np.zeros((0, 3), np.int64),
                {'budget_exceeded': True, 'fidelity_ok': False,
                 'warnings': [], 'seconds': 0.01})

    real = graft_mod.shell_wrap
    graft_mod.shell_wrap = fake_shell_wrap
    try:
        stats = {'stage1': {'holes_remaining': 3,
                            'non_manifold_edges_remaining': 1}}
        after = {'faces_number': 0}
        repair.graft_tier(_FakeML(), after, after, stats,
                          np.zeros((0, 3), np.float64),
                          np.zeros((5, 3), np.int64), '/tmp',
                          graft=True, intensity='balanced',
                          spec=triage.PRESETS['balanced'])
    finally:
        graft_mod.shell_wrap = real
    rec = stats['graft']
    assert seen.get('time_budget') == 300.0
    assert rec['ran'] is True
    assert rec['timed_out'] is True
    assert rec['skipped_budget'] is True
    assert rec['reject_reason'] == 'budget'
    assert rec['holes'] == 3 and rec['non_manifold'] == 1


def test_graft_tier_no_graft_is_a_noop():
    stats = {}
    after = {'faces_number': 0}
    out_ms, out_after = repair.graft_tier(
        _FakeML(), after, after, stats, np.zeros((0, 3), np.float64),
        np.zeros((5, 3), np.int64), '/tmp', graft=False)
    assert stats == {}
    assert out_after is after


if __name__ == "__main__":
    fns = [v for k, v in sorted(globals().items())
           if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
        print(f"ok  {fn.__name__}")
