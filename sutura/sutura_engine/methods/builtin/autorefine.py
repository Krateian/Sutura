# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 5: Autorefine."""
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=5,
    id='autorefine',
    name='Autorefine',
    display_name='Autorefine',
    description='Resolve self-intersections by subdividing intersecting triangles '
                '(Lazard & Valque 2025); never deletes input faces.',
    family='si',
    kwargs={'autorefine': True},
)
