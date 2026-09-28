# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 14: Mirror Complete."""
from sutura_engine.methods.availability import mirror_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=14,
    id='mirror_complete',
    name='Mirror Complete',
    display_name='Mirror Complete',
    description='Complete the missing back of a single-sided scan by mirroring '
                'the visible surface across its detected symmetry plane '
                '(mirror_repair.py); falls back to Poisson when the symmetry '
                'is not confident.',
    family='closing',
    invents_geometry=True,
    guard_input_to_output=True,
    kwargs={'closing': 'mirror', 'deep_repair': 'off', 'ftetwild': False},
    available_fn=mirror_available,
)
