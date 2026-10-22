# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 19: Coaptation (tolerance seam-weld + per-body union).

Rebuilds the ORIGINAL input for a folded / overlapping multi-body soup: the
reload-equivalent float32 soup is split into edge-connected bodies, boundary
("seam") vertices within one float32 ULP of the largest coordinate are welded,
each body is closed and oriented, and the bodies are folded with the exact
``sutura_geom.union_pair``.  The candidate is adopted only when it is STRICTLY
better than the result the ladder holds (reload-watertight, exact
self-intersections 0 and strictly below the baseline, no worse on holes /
non-manifold edges / components); a mesh that does not benefit is returned
byte-identical.  It is ON by default in the deep-repair ladder (after Splint,
before fTetWild).
"""
from sutura_engine.methods.availability import coaptation_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=19,
    id='coaptation',
    name='Coaptation',
    display_name='Coaptation',
    description='Tolerance seam-weld + per-body union: split a folded/overlapping '
                'multi-body soup into bodies, coapt the seam within one float32 '
                'ULP and fold them with the exact union. Adopted only when '
                'strictly better (watertight, 0 self-intersections).',
    family='solid',
    invents_geometry=True,
    # The tier is adopted only on a strict reload-watertight, strictly-better
    # candidate, so the generic one-sided (input -> output) Hausdorff guard --
    # which would also see the legitimate seam closure -- is skipped.
    self_guarded=True,
    core_required=True,
    kwargs={'coaptation': True, 'deep_repair': 'full', 'ftetwild': False},
    available_fn=coaptation_available,
)
