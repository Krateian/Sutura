# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 1: Quick Clean."""
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=1,
    id='quick_clean',
    name='Quick Clean',
    display_name='Quick Clean',
    description='Stage 1 + manifold3d rebuild; no deep repair and no fTetWild.',
    family='clean',
    kwargs={'deep_repair': 'off', 'ftetwild': False},
)
