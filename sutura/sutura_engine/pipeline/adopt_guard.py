# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Shared adoption guard for geometry-inventing tiers and methods.

A tier must not be adopted just because it closed a hole: a candidate can
invent orders of magnitude more geometry or introduce self-intersections.  The
guard has two independent checks:

* face/vertex blow-up - the candidate may not exceed the larger of
  ``face_ratio * input`` or ``input + min_slack + slack_per_unit * slack_units``
  (``slack_units`` is the Flap boundary-loop vertex total or the Tamponade
  ``plug_faces``; volumetric tiers pass 0);
* self-intersection increase - exact ``topology.self_intersecting_faces`` on
  the candidate against the mesh the tier replaces, with a policy per tier
  (``non_rise``, ``strict_zero``, ``flag``, ``improved_flag`` or ``off``).

The module is pure numpy; the SI scan is lazy-imported so a unit test can use
the face/vertex helpers without the Rust kernel.  Every tier and the central
``triage._evaluate`` call ``check_candidate``; the result is a small dataclass
whose ``fields`` are copied into the tier/method report.
"""
from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np

from sutura_engine.policy import GATES as _GATES

_ADOPTION = _GATES['adoption']

FACE_RATIO = float(_ADOPTION['face_ratio'])
VERTEX_RATIO = float(_ADOPTION['vertex_ratio'])
MIN_SLACK = int(_ADOPTION['min_slack'])
LOOP_FACE_SLACK = int(_ADOPTION['loop_face_slack'])
LOOP_VERTEX_SLACK = int(_ADOPTION['loop_vertex_slack'])
# fTetWild's tetrahedral boundary is naturally much denser than the input
# surface; a pure 4x ratio would reject ordinary small-mesh output (71 -> 5,120).
# It gets an absolute floor instead, and a topology-improving result may exceed
# the ceiling as a flag rather than a rejection.
FTETWILD_ABS_FLOOR = int(_ADOPTION['ftetwild_abs_floor'])

SI_POLICIES = ('non_rise', 'strict_zero', 'flag', 'improved_flag', 'off')


@dataclass(frozen=True)
class GuardPolicy:
    face_ratio: float = FACE_RATIO
    vertex_ratio: float = VERTEX_RATIO
    min_slack: int = MIN_SLACK
    loop_face_slack: int = LOOP_FACE_SLACK
    loop_vertex_slack: int = LOOP_VERTEX_SLACK
    si_policy: str = 'non_rise'
    si_rel_tol: float = 0.0
    si_abs_tol: int = 0
    face_abs_floor: int = 0
    vertex_abs_floor: int = 0
    allow_growth_if_improved: bool = False


@dataclass(frozen=True)
class GuardResult:
    ok: bool
    reason: str | None = None
    fields: dict = field(default_factory=dict)


_TIER_POLICIES = {
    # Flap's pre-chain SI rise is a flag, not a rejection: Stage-1/Stage-2 can
    # lower it (and 40886 can raise it further), so the final safety net runs
    # the flap-OFF path and compares the two finals (see service.py).
    'flap': 'flag',
    # Graft follows the same lesson: a candidate that reduces holes/non-manifold
    # edges may raise the exact SI count (37111: 413 -> 739).  Rejecting it left
    # the 10-hole/9-nm baseline in place and the fallback chain timed out; a
    # topology gain therefore flags the rise instead of rejecting the candidate.
    # The face/vertex blow-up cap stays the default 4x (no fTetWild floor).
    'graft': 'improved_flag',
    'splint': 'non_rise',
    'transplant': 'non_rise',
    'transplant_plus': 'non_rise',
    'wall_thicken': 'non_rise',
    # Coaptation already requires zero SI in its own gate.
    'coaptation': 'strict_zero',
    # The post-stage1 Tamponade deliberately reports SI only; its prepass
    # keeps the non-rise gate in tamponade.py.
    'tamponade': 'flag',
    # fTetWild may add SI while it fixes holes/nm; that is a flag, not a
    # rejection (37402: 175 holes -> 0, SI +128).  Without a topology gain
    # the normal non-rise rule applies.
    'ftetwild': 'improved_flag',
    'dressing': 'strict_zero',
    # Methods outside the explicit non-rise list keep the historical
    # report-only SI behaviour; the face/vertex guard still applies to them.
    # Wall Thicken is here because the decision names only Flap/Graft/Splint/
    # Transplant for SI non-rise.
}


_FTETWILD_POLICY = GuardPolicy(
    si_policy='improved_flag', face_abs_floor=FTETWILD_ABS_FLOOR,
    vertex_abs_floor=FTETWILD_ABS_FLOOR, allow_growth_if_improved=True)


def policy_for(tier):
    """Guard policy for a tier/method slug (unknown -> SI flag only)."""
    if str(tier) == 'ftetwild':
        return _FTETWILD_POLICY
    return GuardPolicy(si_policy=_TIER_POLICIES.get(str(tier), 'flag'))


def blowup_reason(input_faces, input_vertices, candidate_faces,
                  candidate_vertices, slack_units=0,
                  policy=GuardPolicy()):
    """Return a face/vertex blow-up reason, or None."""
    units = max(int(slack_units or 0), 0)
    face_slack = int(policy.min_slack + policy.loop_face_slack * units)
    allowed_faces = max(int(policy.face_ratio * int(input_faces)),
                        int(input_faces) + face_slack,
                        int(policy.face_abs_floor))
    if int(candidate_faces) > allowed_faces:
        return ('candidate face blow-up (faces %d -> %d; allowed %d = '
                'max(%g * input, input + %d))'
                % (input_faces, candidate_faces, allowed_faces,
                   policy.face_ratio, face_slack))
    vertex_slack = int(policy.min_slack + policy.loop_vertex_slack * units)
    allowed_vertices = max(int(policy.vertex_ratio * int(input_vertices)),
                           int(input_vertices) + vertex_slack,
                           int(policy.vertex_abs_floor))
    if int(candidate_vertices) > allowed_vertices:
        return ('candidate vertex blow-up (vertices %d -> %d; allowed %d = '
                'max(%g * input, input + %d))'
                % (input_vertices, candidate_vertices, allowed_vertices,
                   policy.vertex_ratio, vertex_slack))
    return None


def exact_si_count(vertices, faces, cap=2_000_000):
    """Exact self-intersection face count, or None when not measurable."""
    try:
        import topology
        v = np.ascontiguousarray(vertices, dtype=np.float64)
        t = np.ascontiguousarray(faces, dtype=np.int64)
        if len(t) < 2:
            return 0
        _mask, count = topology.self_intersecting_faces(v, t)
        count = int(count)
        return count if count >= 0 else None
    except Exception:  # noqa: BLE001 - an unmeasurable SI is reported, not fatal
        return None


def si_reason(base_si, candidate_si, policy=GuardPolicy(),
              improved_topology=False):
    """Return a rejection reason for an SI increase, or None.

    ``improved_flag`` never rejects when the tier also improved holes or
    non-manifold edges; the caller records the increase as a flag.  Without
    that topology gain it falls back to the strict non-rise rule.
    """
    if policy.si_policy in ('off', 'flag'):
        return None
    if base_si is None or candidate_si is None:
        return None
    base_si = int(base_si)
    candidate_si = int(candidate_si)
    if policy.si_policy == 'strict_zero':
        if candidate_si > 0:
            return ('candidate has self-intersections (%d)' % candidate_si)
        return None
    if policy.si_policy == 'improved_flag' and improved_topology:
        return None
    allowed = base_si + max(int(policy.si_abs_tol),
                            int(policy.si_rel_tol * base_si))
    if candidate_si > allowed:
        return ('candidate increases self-intersections (%d -> %d; allowed %d)'
                % (base_si, candidate_si, allowed))
    return None


def check_candidate(input_faces, input_vertices, candidate_faces,
                    candidate_vertices, *, slack_units=0, policy=None,
                    base_si=None, candidate_si=None, base_arrays=None,
                    candidate_arrays=None, measure_si=True,
                    improved_topology=False):
    """Run the blow-up and SI checks and return a GuardResult.

    SI is measured only when the face/vertex check passes and the caller asks
    for it, so a bloated candidate never pays for the exact scan.  The tier
    calls this when the candidate would otherwise be adopted; a rejected
    candidate keeps the previous result.
    """
    policy = policy or GuardPolicy()
    fields = {
        'input_faces': int(input_faces),
        'input_vertices': int(input_vertices),
        'candidate_faces': int(candidate_faces),
        'candidate_vertices': int(candidate_vertices),
        'slack_units': int(slack_units or 0),
        'guard_rejected': None,
        'guard_reason': None,
        'si_policy': policy.si_policy,
        'si_checked': False,
        'si_flagged': False,
        'baseline_self_intersections': (int(base_si)
                                        if base_si is not None else None),
        'candidate_self_intersections': (int(candidate_si)
                                         if candidate_si is not None else None),
        'si_improved_topology': bool(improved_topology),
        'blowup_flagged': False,
    }
    reason = blowup_reason(input_faces, input_vertices, candidate_faces,
                           candidate_vertices, slack_units, policy)
    if reason is not None:
        if policy.allow_growth_if_improved and improved_topology:
            fields['blowup_flagged'] = True
            fields['guard_reason'] = (reason + ' (flagged: holes/non-manifold '
                                      'improved)')
        else:
            fields['guard_rejected'] = reason
            fields['guard_reason'] = reason
            return GuardResult(False, reason, fields)

    if measure_si and policy.si_policy != 'off':
        if base_si is None and base_arrays is not None:
            base_si = exact_si_count(*base_arrays)
            fields['baseline_self_intersections'] = (
                int(base_si) if base_si is not None else None)
        if candidate_si is None and candidate_arrays is not None:
            candidate_si = exact_si_count(*candidate_arrays)
            fields['candidate_self_intersections'] = (
                int(candidate_si) if candidate_si is not None else None)
        fields['si_checked'] = (base_si is not None and candidate_si is not None)

    reason = si_reason(base_si, candidate_si, policy, improved_topology)
    if reason is not None:
        fields['guard_rejected'] = reason
        fields['guard_reason'] = reason
        return GuardResult(False, reason, fields)

    if (base_si is not None and candidate_si is not None
            and int(candidate_si) > int(base_si)
            and policy.si_policy in ('flag', 'improved_flag')):
        fields['si_flagged'] = True
        fields['guard_reason'] = ('self-intersections flagged (%d -> %d)'
                                  % (int(base_si), int(candidate_si)))
    return GuardResult(True, None, fields)
