# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 13: Graft (shell wrap).

Runs the Cast morphology shell-wrap tier (``sutura_engine.graft``) on the
original input: a generalized-winding signed-distance envelope (dilate -> true
EDT re-distance -> erode), a projection onto the healthy original and a
verbatim hybrid that keeps the healthy triangles and closes only the damaged
region.  The output is adopted only when it is reload-watertight; the report's
``graft.fidelity_ok`` / ``graft.warnings`` carry the detail-loss verdict the
triage policy reads.
"""
from sutura_engine.methods.availability import graft_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=13,
    id='graft',
    name='Graft',
    display_name='Graft',
    description='Morphology shell wrap: close the damaged region with a '
                'generalized-winding EDT envelope, keep the healthy triangles '
                'verbatim, and report the detail loss. Reload-watertight.',
    family='envelope',
    invents_geometry=True,
    # Graft enforces its own healthy original -> result fidelity guard, so the
    # generic one-sided (input -> output) Hausdorff guard -- which also sees
    # the caps that legitimately close the openings -- is skipped.
    self_guarded=True,
    kwargs={'graft': True, 'deep_repair': 'off', 'ftetwild': False},
    available_fn=graft_available,
)
