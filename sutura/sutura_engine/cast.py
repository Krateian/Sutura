# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""sutura_engine.cast - Thin Python wrapper around the Rust `sutura_geom` extension.

Exposes exact geometric predicates and arrangement-lite routines implemented in
Rust (sutura-geom crate) with Shewchuk adaptive-precision arithmetic and exact
rational representations.

Provides:
  - is_available(): True if sutura_geom native extension is importable
  - version(): Version string of sutura_geom or None
  - orient3d(): Exact sign of oriented volume of a tetrahedron
  - insphere(): Exact sign of whether a point lies within a sphere
  - arrangement_lite(): Exact self-intersection split on triangle meshes
  - raystab_points(): Ray-stabbing inside/outside vote for arbitrary points
  - raystab_grid(): Full-grid ray-stabbing inside mask (analysis/tests)
  - set_cdt_experimental(): Developer hook for experimental CDT options
"""
from typing import Any, Dict, Optional, Tuple
import numpy as np

try:
    import sutura_geom as _geom
    _AVAILABLE = True
except ImportError:
    _geom = None
    _AVAILABLE = False


def is_available() -> bool:
    """Return True if the sutura_geom Rust extension is installed and importable."""
    return _AVAILABLE


def version() -> Optional[str]:
    """Return the sutura_geom version string, or None if unavailable."""
    if _geom is not None and hasattr(_geom, '__version__'):
        return str(_geom.__version__)
    return None


def _require_geom():
    if not _AVAILABLE or _geom is None:
        raise RuntimeError(
            "sutura_geom Rust extension is not available in the active environment. "
            "Build or install rust/sutura-geom to enable exact geometry predicates."
        )


def orient3d(pa: Any, pb: Any, pc: Any, pd: Any) -> float:
    """Sign of the oriented volume of tetrahedron (pa, pb, pc, pd).

    Returns positive when pd lies below the plane through counterclockwise (pa, pb, pc),
    negative when pd lies above, and exactly 0.0 when coplanar.
    """
    _require_geom()
    return _geom.orient3d(pa, pb, pc, pd)


def insphere(pa: Any, pb: Any, pc: Any, pd: Any, pe: Any) -> float:
    """Sign of whether pe lies inside the sphere through (pa, pb, pc, pd).

    Returns positive when pe is inside the sphere, negative when outside,
    and exactly 0.0 when cospherical.
    """
    _require_geom()
    return _geom.insphere(pa, pb, pc, pd, pe)


def arrangement_lite(verts: np.ndarray, tris: np.ndarray) -> Tuple[np.ndarray, np.ndarray, Dict[str, Any]]:
    """Split self-intersecting triangle soup into non-intersecting faces using exact predicates.

    Returns:
        (out_verts, out_tris, report)
    """
    _require_geom()
    out_v, out_t, rep = _geom.arrangement_lite(verts, tris)
    return np.asarray(out_v, dtype=np.float64), np.asarray(out_t, dtype=np.int32), dict(rep)


def set_cdt_experimental(bits: int) -> int:
    """Set developer hook flags for experimental CDT behaviours. Returns previous value."""
    _require_geom()
    return _geom._set_cdt_experimental(bits)


def raystab_points(verts: np.ndarray, tris: np.ndarray, points: np.ndarray,
                   n_dirs: Optional[int] = None, seed: int = 0,
                   parity: bool = False,
                   escape_weight: float = 2.0) -> Tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray]:
    """Ray-stabbing inside/outside vote at query points.

    The vote is the oriented net crossing count with a parity fallback for
    orientation-inconsistent rays; ``parity=True`` forces parity everywhere.

    Returns:
        (inside, inside_votes, outside_votes, escape_votes)
    """
    _require_geom()
    return _geom.raystab_points(verts, tris, points, n_dirs=n_dirs, seed=seed,
                                parity=parity, escape_weight=escape_weight)


def raystab_grid(verts: np.ndarray, tris: np.ndarray, voxel: Optional[float] = None,
                 box: Optional[np.ndarray] = None, n_dirs: Optional[int] = None,
                 seed: int = 0, parity: bool = False,
                 escape_weight: float = 2.0) -> Tuple[np.ndarray, np.ndarray, Dict[str, Any]]:
    """Full-grid ray-stabbing inside mask and vote score.

    Returns:
        (inside, score, info)
    """
    _require_geom()
    return _geom.raystab_grid(verts, tris, voxel=voxel, box=box, n_dirs=n_dirs,
                              seed=seed, parity=parity, escape_weight=escape_weight)


__all__ = [
    'is_available',
    'version',
    'orient3d',
    'insphere',
    'arrangement_lite',
    'raystab_points',
    'raystab_grid',
    'set_cdt_experimental',
]
