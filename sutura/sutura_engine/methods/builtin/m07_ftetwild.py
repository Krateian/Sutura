# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 7: fTetWild envelope."""
from sutura_engine.methods.availability import ftetwild_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=7,
    id='ftetwild',
    name='fTetWild envelope',
    description='Tetrahedralize the original surface and extract a watertight, SI-free '
                'boundary (fTetWild). Estimates a back surface.',
    family='envelope',
    invents_geometry=True,
    kwargs={'ftetwild': True},
    available_fn=ftetwild_available,
)
