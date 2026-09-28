# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 3: Full deep repair."""
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=3,
    id='deep_full',
    name='Full deep repair',
    description="Deep-repair ladder 'full' (today's default balanced behaviour).",
    family='topology',
    kwargs={'deep_repair': 'full', 'ftetwild': 'auto'},
)
