# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 17: Tamponade (interior hole plugging and SI melting).

Runs the Tamponade tier (``sutura_engine.tamponade``, Rust
``sutura_geom.tamponade``) as a pre-pass on the cleaned input before Flap and
Stage 1 hole closing, with a second chance after Flap and before Graft/fTetWild:
each open boundary loop is plugged from the inside without touching the
surrounding healthy triangles, and residual self-intersections are melted in
thin, individually rolled-back layers. No input triangle is ever deleted
(``healthy_faces_kept`` counts them), which is why it is a geometry-inventing
tier adopted only when the reloaded holes + non-manifold count strictly falls.
On by default (disable with ``--no-tamponade`` / ``SUTURA_TAMPONADE=0``).
"""
from sutura_engine.methods.availability import tamponade_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=17,
    id='tamponade',
    name='Tamponade',
    display_name='Tamponade',
    description='Plug open boundaries from the inside and melt residual '
                'self-intersections in rolled-back layers. Never deletes an '
                'input triangle; adopted only when the reload-honest holes + '
                'non-manifold count strictly falls; on by default.',
    family='interior',
    invents_geometry=True,
    # The tier's own hole/non-manifold/healthy-face guard is authoritative, so
    # the generic one-sided (input -> output) Hausdorff guard is skipped.
    self_guarded=True,
    # On by default in the pipeline, but still excluded from the auto ranking
    # (needs_user_input): the interior plug is picked explicitly
    # (--methods 17 / --tamponade) as an alternative to the default pipeline.
    needs_user_input=True,
    kwargs={'tamponade': True, 'tamponade_force': True,
            'deep_repair': 'off', 'ftetwild': False},
    available_fn=tamponade_available,
)
