# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 17: Tamponade (interior hole plugging and SI melting).

Runs the Tamponade tier (``sutura_engine.tamponade``, Rust
``sutura_geom.tamponade``) on the stage-1 result: each open boundary loop is
plugged from the inside without touching the surrounding healthy triangles, and
residual self-intersections are melted in thin, individually rolled-back layers.
No input triangle is ever deleted (``healthy_faces_kept`` counts them), which is
why it is a geometry-inventing tier adopted only when the reloaded holes +
non-manifold count strictly falls.  Opt-in until measured on the corpus.
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
                'non-manifold count strictly falls; opt-in.',
    family='interior',
    invents_geometry=True,
    # The tier's own hole/non-manifold/healthy-face guard is authoritative, so
    # the generic one-sided (input -> output) Hausdorff guard is skipped.
    self_guarded=True,
    # Opt-in until measured on the corpus: excluded from the auto ranking,
    # chosen explicitly (--methods 17 or --tamponade).
    needs_user_input=True,
    kwargs={'tamponade': True, 'deep_repair': 'off', 'ftetwild': False},
    available_fn=tamponade_available,
)
