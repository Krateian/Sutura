# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 16: Dressing (variable-viscosity volumetric skinning).

Runs the Dressing tier (``sutura_engine.dressing``) on the original input: a
variable-thickness isosurface of the generalized-winding signed field,
``F(x) = s(x) - r(x)``, where the viscosity radius ``r`` is thin over detailed
healthy surface and thicker over damage.  The isosurface of a scalar field is
2-manifold and self-intersection-free by construction, which targets whole-shell
topological folds and complex open damage.  It is opt-in (``needs_user_input``)
until measured on the corpus.
"""
from sutura_engine.methods.availability import dressing_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=16,
    id='dressing',
    name='Dressing',
    display_name='Dressing',
    description='Variable-viscosity volumetric skinning: wraps self-'
                'intersections, folds and open boundaries in a narrow-band '
                'generalized-winding isosurface. 2-manifold and '
                'self-intersection-free by construction; opt-in.',
    family='envelope',
    invents_geometry=True,
    # Dressing enforces its own coat->input fidelity guard, so the generic
    # one-sided (input -> output) Hausdorff guard is skipped.
    self_guarded=True,
    # Opt-in until measured on the corpus: excluded from the auto ranking
    # exactly like Wall Thicken (#15), chosen explicitly (--methods 16 or
    # --experimental-dressing).
    needs_user_input=True,
    kwargs={'dressing': True, 'deep_repair': 'off', 'ftetwild': False},
    available_fn=dressing_available,
)
