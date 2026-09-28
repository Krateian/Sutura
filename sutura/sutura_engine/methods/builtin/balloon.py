# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 8: Balloon."""
from sutura_engine.methods.availability import closing_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=8,
    id='balloon',
    name='Balloon',
    display_name='Balloon',
    description='Screened-Poisson surface reconstruction that closes the large opening '
                'of a single-sided scan (estimates the missing back surface).',
    family='closing',
    invents_geometry=True,
    guard_input_to_output=True,
    kwargs={'closing': 'poisson', 'deep_repair': 'off', 'ftetwild': False},
    available_fn=closing_available,
)
