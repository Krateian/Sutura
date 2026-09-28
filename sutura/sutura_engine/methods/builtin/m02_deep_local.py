# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 2: Local deep repair."""
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=2,
    id='deep_local',
    name='Local deep repair',
    description="Deep-repair ladder 'local': re-mesh only the damaged regions.",
    family='topology',
    kwargs={'deep_repair': 'local', 'ftetwild': False},
)
