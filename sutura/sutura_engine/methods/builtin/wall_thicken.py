# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 15: Wall Thicken."""
from sutura_engine.methods.availability import wall_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=15,
    id='wall_thicken',
    name='Wall Thicken',
    display_name='Wall Thicken',
    description='Opt-in thin-wall repair: measure per-vertex wall thickness '
                'from an SDF grid and thicken walls below the target by a '
                'morphological closing of the solid (wall_thickness.py). '
                'Never runs automatically; select it explicitly.',
    family='envelope',
    invents_geometry=True,
    needs_user_input=True,
    guard_input_to_output=True,
    kwargs={'wall_thicken': True, 'deep_repair': 'off', 'ftetwild': False},
    available_fn=wall_available,
)
