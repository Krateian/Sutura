# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 1: Fast cleanup."""
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=1,
    id='fast',
    name='Fast cleanup',
    description='Stage 1 + manifold3d rebuild; no deep repair and no fTetWild.',
    family='clean',
    kwargs={'deep_repair': 'off', 'ftetwild': False},
)
