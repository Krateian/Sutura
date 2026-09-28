# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 2: Local Mend."""
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=2,
    id='local_mend',
    name='Local Mend',
    display_name='Local Mend',
    description="Deep-repair ladder 'local': re-mesh only the damaged regions.",
    family='topology',
    kwargs={'deep_repair': 'local', 'ftetwild': False},
)
