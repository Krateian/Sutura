# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Deterministic work estimates for the automatic fallback ladder.

Every value here is a pure function of the input geometry and the intensity
preset (never the wall clock), so the skip/adopt decision is identical on every
machine and under any CPU load.  The estimates are deliberately coarse: they
only need to separate the meshes whose fallback tiers cannot finish in a
reasonable amount of work from the ones that can.

Units
-----
``damage_work``
    Weighted input faces: ``faces * (1 + nm_edges + nm_vertices + holes)``.
    A non-manifold edge/vertex or an open boundary makes almost every tier
    revisit the same faces, so it multiplies the cost.
``grid_work``
    Kilo cell-face units of one morphology grid attempt:
    ``ceil(cells * faces / 1000)``.  A ``morph_close`` queried the BVH at every
    grid cell, so the cost scales with both the cell count and the face count.
``si_pair_work``
    Candidate exact-self-intersection pair work:
    ``faces * (1 + nm_edges + nm_vertices + holes)``.

Opt out with ``SUTURA_FALLBACK_BUDGET=0`` (fallback estimate) and
``SUTURA_SI_EXACT_BUDGET=0`` (exact-SI cap).  A positive integer overrides the
default cap; an invalid value keeps the default.
"""

from __future__ import annotations

import os

DEFAULT_FALLBACK_WORK_BUDGET = 6_000_000
DEFAULT_SI_WORK_BUDGET = 15_000_000
# One morphology attempt is charged in kilo cell-face units.  The Graft/Dressing
# pre-gate has its own calibrated cap because a successful Graft on a large
# mesh is legitimate work (a 98 M-unit candidate is adopted on the reference
# corpus), while the 140 M+ attempts are the ones that never finish.
DEFAULT_GRAFT_GRID_WORK_BUDGET = 120_000_000

FALLBACK_BUDGET_ENV = 'SUTURA_FALLBACK_BUDGET'
SI_BUDGET_ENV = 'SUTURA_SI_EXACT_BUDGET'
GRAFT_GRID_BUDGET_ENV = 'SUTURA_GRAFT_GRID_BUDGET'

_GRID_WORK_SCALE = 1000

_FALSE = {'0', 'false', 'no', 'off'}


def _resolve_budget(environ, key, default):
    raw = (os.environ if environ is None else environ).get(key)
    if raw is None or str(raw).strip() == '':
        return int(default)
    text = str(raw).strip().lower()
    if text in _FALSE:
        return None
    try:
        value = int(text)
    except (TypeError, ValueError):
        return int(default)
    return value if value > 0 else None


def fallback_work_budget(environ=None):
    """Fallback estimate cap (``None`` = unlimited / opt-out)."""
    return _resolve_budget(environ, FALLBACK_BUDGET_ENV,
                           DEFAULT_FALLBACK_WORK_BUDGET)


def si_work_budget(environ=None):
    """Exact-SI pair-work cap (``None`` = unlimited / opt-out)."""
    return _resolve_budget(environ, SI_BUDGET_ENV, DEFAULT_SI_WORK_BUDGET)


def graft_grid_work_budget(environ=None):
    """One-attempt morphology grid cap (``None`` = unlimited / opt-out)."""
    return _resolve_budget(environ, GRAFT_GRID_BUDGET_ENV,
                           DEFAULT_GRAFT_GRID_WORK_BUDGET)


def damage_work(faces, non_manifold_edges=0, non_manifold_vertices=0, holes=0):
    """Weighted face count used for the generic fallback estimate."""
    faces = max(0, int(faces))
    damage = (max(0, int(non_manifold_edges))
              + max(0, int(non_manifold_vertices))
              + max(0, int(holes)))
    return int(faces) * (1 + damage)


def grid_work(faces, cells):
    """Kilo cell-face work of one morphology grid attempt."""
    faces = max(0, int(faces))
    cells = max(0, int(cells))
    return (faces * cells + _GRID_WORK_SCALE - 1) // _GRID_WORK_SCALE


def grid_work_with_holes(faces, cells, holes=0):
    """Grid work plus one extra attempt per 100 open holes.

    A hole is revisited by every rung of the closing ladder and by the
    projection/hybrid decision, so a hole-rich mesh costs more than the raw
    grid-at-a-cell count suggests.  Pure and deterministic.
    """
    return grid_work(faces, cells) * (1 + max(0, int(holes)) // 100)


def si_pair_work(faces, non_manifold_edges=0, non_manifold_vertices=0,
                 holes=0):
    """Exact-SI pair-work estimate for one mesh.

    Open boundaries count as damage too: the classifier still has to test the
    triangles on both sides of every hole.
    """
    faces = max(0, int(faces))
    damage = (max(0, int(non_manifold_edges))
              + max(0, int(non_manifold_vertices))
              + max(0, int(holes)))
    return int(faces) * (1 + damage)


_GRID_METHODS = {'graft', 'dressing'}

# Per-method cost class.  A cheap local/topology tier must not be skipped just
# because a global rebuild on the same damaged mesh would be too expensive:
# only the method's own estimate is compared with its own budget.
#   faces  - a rerun of the whole pipeline driven by the input faces
#   damage - a global rebuild driven by the damage counters
#   grid   - a whole-mesh morphology grid
_METHOD_WORK_CLASS = {
    'quick_clean': 'faces',
    'local_mend': 'faces',
    'join': 'faces',
    'transplant': 'faces',
    'transplant_plus': 'faces',
    'autorefine': 'damage',
    'exact_refine': 'damage',
    'ftetwild': 'damage',
    'balloon': 'damage',
    'backplate': 'damage',
    'scaffold': 'damage',
    'mirror_complete': 'damage',
    'wall_thicken': 'damage',
    'tamponade': 'damage',
    'splint': 'damage',
    'coaptation': 'damage',
    'graft': 'grid',
    'dressing': 'grid',
}

# A fallback is not a one-pass edit: ``_attempt`` calls the whole repair
# pipeline again (tamponade/flap prepasses, filters, stage 2).  Charging a
# single face pass let a 2.6 M-face scan start another ~15-minute run under a
# 6 M budget.  Charge the pipeline's fixed face passes instead; the value is
# deliberately conservative-low (the real cost is higher), but it separates a
# 100 k-face mesh (still allowed) from a multi-million-face one.
_PIPELINE_FACE_COST = 8


def work_class(method_id):
    """Cost class of a fallback method (unknown ids stay on ``damage``)."""
    return _METHOD_WORK_CLASS.get(str(method_id), 'damage')


def candidate_work(method_id, *, damage=0, grid=0, faces=0):
    """Estimate for one ranked fallback candidate.

    Graft and Dressing run a whole-mesh morphology grid; their cost is the
    grid work.  Cheap topology/clean tiers still rerun the whole pipeline, so
    they are charged the pipeline face passes.  Every other tier is a global
    rebuild bounded by its own internal budget and is estimated from the
    weighted input damage.
    """
    cls = work_class(method_id)
    if cls == 'grid' and int(grid) > 0:
        return int(grid)
    if cls == 'faces':
        if int(faces) > 0:
            return int(faces) * _PIPELINE_FACE_COST
        return int(damage)
    return int(damage)


def fits_budget(work, budget):
    """Whether ``work`` fits ``budget`` (``None`` = unlimited)."""
    return True if budget is None else int(work) <= int(budget)
