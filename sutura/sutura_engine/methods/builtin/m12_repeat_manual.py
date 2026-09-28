# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 12: Repeat-aware manual."""
from sutura_engine.methods.availability import repeat_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=12,
    id='repeat_manual',
    name='Repeat-aware manual',
    description='Transplant the repeated element marked by the user (--repeat-source) onto '
                'the damaged one (--repeat-target); never runs automatically.',
    family='pattern',
    invents_geometry=True,
    self_guarded=True,
    needs_user_input=True,
    kwargs={'repeat': 'manual', 'deep_repair': 'off', 'ftetwild': False},
    available_fn=repeat_available,
)
