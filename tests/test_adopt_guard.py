#!/usr/bin/env python3
"""Regression tests for the shared adoption guard (pure helper).

Runnable directly with numpy (the exact-SI scan is lazy and covered by the
tier/integration tests):
    <repair-venv>/bin/python tests/test_adopt_guard.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for path in (SUTURA, REPO):
    if path not in sys.path:
        sys.path.insert(0, path)

import numpy as np  # noqa: E402
from sutura_engine.pipeline.adopt_guard import (  # noqa: E402
    GuardPolicy, blowup_reason, check_candidate, si_reason,
)
from sutura_engine.triage import _adopt_guard_for_method  # noqa: E402


def main():
    # Measured Flap blow-up: 6,073 / 3,077 -> 303,335 / 151,675, 93 loop verts.
    reason = blowup_reason(6073, 3077, 303335, 151675, 93)
    assert reason is not None and 'face' in reason, reason

    # Small local refinement and a modest patch on a large input stay allowed.
    assert blowup_reason(12, 8, 40, 24, 4) is None
    assert blowup_reason(90000, 45000, 95000, 47000, 100) is None

    # The loop slack is a real allowance, but its ceiling is still the ratio.
    assert blowup_reason(1000, 500, 2856, 2000, 100) is None
    assert blowup_reason(1000, 500, 5000, 2000, 100) is not None
    # A volumetric tier gets only the 4x + 256 floor, never the loop term.
    assert blowup_reason(1000, 500, 2000, 1000, 0) is None
    assert blowup_reason(1000, 500, 5000, 1000, 0) is not None

    # SI policies.
    p = GuardPolicy(si_policy='non_rise')
    assert si_reason(10, 11, p) is not None
    assert si_reason(10, 10, p) is None
    assert si_reason(0, 1, GuardPolicy(si_policy='strict_zero')) is not None
    assert si_reason(100, 0, GuardPolicy(si_policy='strict_zero')) is None
    assert si_reason(0, 5, GuardPolicy(si_policy='flag')) is None
    ip = GuardPolicy(si_policy='improved_flag')
    assert si_reason(0, 5, ip, improved_topology=True) is None
    assert si_reason(0, 5, ip, improved_topology=False) is not None
    rp = GuardPolicy(si_policy='relative', si_rel_tol=0.1)
    assert si_reason(100, 110, rp) is None
    assert si_reason(100, 111, rp) is not None

    # fTetWild's natural tetrahedral density is allowed by the absolute floor,
    # and a topology-improving growth is a flag, not a rejection.
    fp = GuardPolicy(si_policy='improved_flag', face_abs_floor=50_000,
                     vertex_abs_floor=50_000, allow_growth_if_improved=True)
    assert blowup_reason(71, 47, 5120, 2562, 0, fp) is None
    assert blowup_reason(71, 47, 60_000, 30_000, 0, fp) is not None
    grow = check_candidate(71, 47, 60_000, 30_000, measure_si=False,
                           improved_topology=True, policy=fp)
    assert grow.ok and grow.fields['blowup_flagged'] is True, grow
    no_grow = check_candidate(71, 47, 60_000, 30_000, measure_si=False,
                              improved_topology=False, policy=fp)
    assert not no_grow.ok and 'blow-up' in no_grow.reason, no_grow

    # Graft follows fTetWild: a holes/nm gain with an SI rise is a flag, not a
    # rejection (37111: holes 10 -> 0, nm 9 -> 0, SI 413 -> 739).  Without the
    # topology gain the non-rise rule still rejects, and the default 4x
    # face/vertex cap stays (no fTetWild floor / no growth allowance).
    from sutura_engine.pipeline.adopt_guard import policy_for
    gp = policy_for('graft')
    assert gp.si_policy == 'improved_flag', gp
    assert gp.face_abs_floor == 0 and gp.allow_growth_if_improved is False, gp
    assert si_reason(413, 739, gp, improved_topology=True) is None
    assert si_reason(413, 739, gp, improved_topology=False) is not None
    grow_cap = check_candidate(1000, 500, 5000, 600, measure_si=False,
                               improved_topology=True, policy=gp)
    assert not grow_cap.ok and 'face blow-up' in grow_cap.reason, grow_cap

    # check_candidate: a pass, a blow-up reject and a flag record.
    ok = check_candidate(1000, 500, 1200, 600, base_si=0, candidate_si=0,
                         measure_si=False, policy=p)
    assert ok.ok and ok.fields['si_checked'] is False
    bad = check_candidate(1000, 500, 9000, 600, measure_si=False, policy=p)
    assert not bad.ok and 'face blow-up' in bad.reason
    flagged = check_candidate(1000, 500, 1200, 600, base_si=0, candidate_si=4,
                              measure_si=False,
                              policy=GuardPolicy(si_policy='flag'))
    assert flagged.ok and flagged.fields['si_flagged'] is True

    # The central method evaluator uses the same helper: a Graft/Transplant/
    # Wall-Thicken output 100x the input must be rejected even though those
    # methods are self-guarded (the generic Hausdorff guard is skipped).
    class _Method:
        id = 'graft'
        invents_geometry = True
        self_guarded = True

    cube_v = np.asarray([(x, y, z) for x in (0, 1) for y in (0, 1)
                         for z in (0, 1)], np.float64)
    cube_f = np.asarray([(0, 1, 3), (0, 3, 2), (4, 6, 7), (4, 7, 5),
                         (0, 4, 5), (0, 5, 1), (2, 3, 7), (2, 7, 6),
                         (0, 2, 6), (0, 6, 4), (1, 5, 7), (1, 7, 3)],
                        np.int64)
    in_objs = [('a', cube_v, cube_f)]
    out_objs = [('a', cube_v, np.tile(cube_f, (100, 1)))]
    central = _adopt_guard_for_method({}, in_objs, out_objs, _Method(), 0, 0)
    assert not central.ok and 'face blow-up' in central.reason, central

    print('test_adopt_guard: ok')


if __name__ == '__main__':
    main()
