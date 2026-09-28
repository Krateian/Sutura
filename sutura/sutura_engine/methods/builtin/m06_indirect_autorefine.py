# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 6: Indirect autorefine (SI, exact)."""
from sutura_engine.methods.availability import indirect_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=6,
    id='indirect_autorefine',
    name='Indirect autorefine (SI, exact)',
    description='Exact arrangement-lite self-intersection split via the rust/sutura-geom '
                'extension (indirect predicates).',
    family='si',
    kwargs={'indirect_autorefine': True},
    available_fn=indirect_available,
)
