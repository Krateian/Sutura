# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Sutura repair pipeline package (P1a mechanical split of sutura/repair.py).

This package is the canonical implementation; ``sutura/repair.py`` is a thin
compatibility shim that re-exports these symbols.

The package ``__init__`` is also the facade returned by
``sutura_engine.triage._repair_mod()`` / ``methods.protocol._repair_mod()``:
it re-exports the handful of entry points those callers need, so the CLI loads
one implementation module instead of a second copy of repair.py.
"""
from defects import detect as detect_defects

from sutura_engine.pipeline.mesh_io import (
    aggregate_triangle_attributes, build_mesh_block, load_mesh, load_meshes,
    parse_3mf_mesh_blocks, parse_3mf_meshes,
    rewrite_3mf_meshes, save_mesh,
)
from sutura_engine.pipeline.pipeline import repair_mesh_from_arrays
from sutura_engine.pipeline.p_weld import (
    enforce_reload_verdict, p_weld_final, reload_strict_holes_nm,
    reload_topology,
    weld_reload_equivalent,
)
from sutura_engine.pipeline.presets import (
    HAUSDORFF_SAMPLES, ExtremeRemovedAllError,
)
from sutura_engine.pipeline.service import repair_3mf, repair_file
from sutura_engine.pipeline.stage2 import maybe_run_stage2
from sutura_engine.pipeline.tiers.ftetwild import ftetwild_available
from sutura_engine.pipeline.utils import _hausdorff_rel
