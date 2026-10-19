# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Adapter for the Manifold kernel (Apache-2.0).

Manifold now lives in the Rust core (`sutura_geom.stage2_rebuild` /
`watertight_check`); there is no Python `manifold3d` import or subprocess bridge any more.
"""


def is_manifold_available() -> bool:
    """Return True when the Rust Manifold bindings are present."""
    try:
        import sutura_geom
        return (hasattr(sutura_geom, 'stage2_rebuild')
                and hasattr(sutura_geom, 'watertight_check'))
    except Exception:  # noqa: BLE001 - availability probe must never raise
        return False
