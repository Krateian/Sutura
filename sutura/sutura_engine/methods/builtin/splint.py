# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Method 18: Splint (exact solid rebuild).

Runs the Rust Splint core (``sutura_geom.splint_cut``) on the original input: a
UDF-proxy surface is Delaunay-tetrahedralised and the interior is cut away,
producing a solid boundary. Splint is the last-resort exact tier of the
deep-repair ladder (after Dressing, before fTetWild) and is adopted only when it
reloads strict-watertight. It is opt-in (``SPLINT_DEFAULT_ENABLED=False``).
"""
from sutura_engine.methods.availability import splint_available
from sutura_engine.methods.protocol import RepairMethod

METHOD = RepairMethod(
    num=18,
    id='splint',
    name='Splint',
    display_name='Splint',
    description='Exact solid rebuild: proxy a UDF surface, Delaunay-'
                'tetrahedralise it and cut away the interior to produce a '
                'watertight solid boundary. Deterministic, addon-free; adopted '
                'only when it reloads strict-watertight.',
    family='solid',
    invents_geometry=True,
    # Splint is adopted only on a strict reload-watertight candidate, so the
    # generic one-sided (input -> output) Hausdorff guard -- which would also
    # see the legitimate caps that close the openings -- is skipped.
    self_guarded=True,
    kwargs={'splint': True, 'deep_repair': 'full', 'ftetwild': False},
    available_fn=splint_available,
)
