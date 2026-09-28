# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 11: Repeat-aware auto."""
from sutura_engine.methods.availability import repeat_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=11,
    id='repeat_auto',
    name='Repeat-aware auto',
    description='Detect a rotational/translational/helical repeated pattern and transplant '
                'a healthy copy onto each damaged or missing one (repeat_repair.py).',
    family='pattern',
    invents_geometry=True,
    self_guarded=True,
    kwargs={'repeat': 'auto', 'deep_repair': 'off', 'ftetwild': False},
    available_fn=repeat_available,
)
