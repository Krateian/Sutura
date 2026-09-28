# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 10: Proxy template match."""
from sutura_engine.methods.availability import proxy_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=10,
    id='proxy_template',
    name='Proxy template match',
    description='Rebuild a heavily broken mesh from a coarse watertight proxy, '
                're-projecting the healthy original regions (proxy_repair.py).',
    family='template',
    invents_geometry=True,
    guard_input_to_output=True,
    kwargs={'proxy_template': True, 'deep_repair': 'off', 'ftetwild': False},
    available_fn=proxy_available,
)
