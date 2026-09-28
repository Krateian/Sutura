# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 11 & 12: Transplant and Transplant+."""
from sutura_engine.methods.availability import repeat_available
from sutura_engine.methods.protocol import RepairMethod

METHOD_AUTO = RepairMethod(
    num=11,
    id='transplant',
    name='Transplant',
    display_name='Transplant',
    description='Detect a rotational/translational/helical repeated pattern and transplant '
                'a healthy copy onto each damaged or missing one (repeat_repair.py).',
    family='pattern',
    invents_geometry=True,
    self_guarded=True,
    kwargs={'repeat': 'auto', 'deep_repair': 'off', 'ftetwild': False},
    available_fn=repeat_available,
)

METHOD_MANUAL = RepairMethod(
    num=12,
    id='transplant_plus',
    name='Transplant+',
    display_name='Transplant+',
    description='Transplant the repeated element marked by the user (--repeat-source) onto '
                'the damaged one (--repeat-target); never runs automatically.',
    family='pattern',
    invents_geometry=True,
    self_guarded=True,
    needs_user_input=True,
    kwargs={'repeat': 'manual', 'deep_repair': 'off', 'ftetwild': False},
    available_fn=repeat_available,
)

METHODS = [METHOD_AUTO, METHOD_MANUAL]
METHOD = METHOD_AUTO
