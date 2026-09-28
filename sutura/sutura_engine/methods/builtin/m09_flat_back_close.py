# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 9: Flat-back close."""
from sutura_engine.methods.availability import closing_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=9,
    id='flat_back_close',
    name='Flat-back close',
    description='Close a single dominant opening of a relief / plate with a flat back '
                'surface and side walls (the input surface is preserved exactly).',
    family='closing',
    invents_geometry=True,
    guard_input_to_output=True,
    kwargs={'closing': 'flat_back', 'deep_repair': 'off', 'ftetwild': False},
    available_fn=closing_available,
)
