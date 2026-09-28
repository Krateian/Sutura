# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 4: Join."""
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=4,
    id='join',
    name='Join',
    display_name='Join',
    description='Move small connected components onto the nearest larger component '
                'instead of deleting them.',
    family='topology',
    kwargs={'join_components': True},
)
