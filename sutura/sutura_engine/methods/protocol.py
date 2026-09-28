# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Protocol and data structures for Sutura repair methods.

Enables an extensible plugin architecture where repair methods are independent
modules conforming to the RepairMethod protocol.
"""
from collections import namedtuple
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, Optional, Protocol, Tuple, runtime_checkable

import numpy as np

Recommendation = namedtuple(
    'Recommendation', 'num id name score reason template reason_key reason_args')
# Backward-compatible defaults: older 6-field constructions stay valid.
Recommendation.__new__.__defaults__ = (None, ())


def _repair_mod():
    """Lazy import so ``repair.py`` can import ``methods`` at module top."""
    import repair
    return repair


@dataclass
class RepairContext:
    """Execution context and parameters provided to a RepairMethod."""
    src_path: Optional[str] = None
    verts: Optional[np.ndarray] = None
    tris: Optional[np.ndarray] = None
    tmpdir: str = ""
    mode: str = "auto"
    profile: Optional[str] = None
    deep_repair: str = "full"
    ftetwild: str = "auto"
    engine: str = "experimental"
    params: Dict[str, Any] = field(default_factory=dict)
    declared_unit: Optional[str] = None
    extra: Dict[str, Any] = field(default_factory=dict)


@dataclass
class MethodResult:
    """Outcome of running a RepairMethod."""
    ok: bool
    verts: Optional[np.ndarray] = None
    tris: Optional[np.ndarray] = None
    report: Dict[str, Any] = field(default_factory=dict)
    error: Optional[str] = None


@runtime_checkable
class RepairMethodProtocol(Protocol):
    """Protocol for an extensible Sutura repair method plugin."""

    num: int
    id: str
    name: str
    display_name: str
    family: str
    description: str
    invents_geometry: bool

    def available(self) -> Tuple[bool, Optional[str]]:
        """Return (is_available, failure_reason)."""
        ...

    def repair(self, ctx: RepairContext) -> MethodResult:
        """Execute the method on the provided context and return MethodResult."""
        ...


@dataclass(frozen=True)
class RepairMethod:
    """Standard Sutura repair method implementation conforming to RepairMethodProtocol."""
    num: int
    id: str
    name: str
    description: str
    family: str
    display_name: str = ""
    invents_geometry: bool = False
    needs_user_input: bool = False
    kwargs: dict = field(default_factory=dict, compare=False)
    available_fn: Optional[Callable] = field(default=None, compare=False, repr=False)
    guard_input_to_output: bool = field(default=False, compare=False, repr=False)
    self_guarded: bool = field(default=False, compare=False, repr=False)

    def __post_init__(self):
        if not self.display_name:
            object.__setattr__(self, 'display_name', self.name)

    def available(self) -> Tuple[bool, Optional[str]]:
        """``(bool, reason|None)`` availability, never raising."""
        if self.available_fn is None:
            return True, None
        try:
            ok, reason = self.available_fn()
            return bool(ok), (reason if not ok else None)
        except Exception as e:
            return False, 'availability check failed: %s' % e

    def score(self, analysis: Any):
        """``(0..1, reason, reason_key, reason_args)`` for the analyzed object."""
        from sutura_engine.methods import _score
        return _score(self.id, analysis)

    def run(self, verts: np.ndarray, tris: np.ndarray, tmpdir: str, ctx: Optional[dict] = None) -> dict:
        """Array-level thin wrapper over the existing pipeline.

        Returns the repair report with the output arrays attached under the
        transient ``_verts``/``_tris`` keys. ``ctx`` is the base
        ``repair_mesh_from_arrays`` kwargs dict; the method's ``kwargs``
        override it.
        """
        repair = _repair_mod()
        merged = dict(ctx or {})
        merged.update(self.kwargs)
        mode = merged.pop('mode', 'auto')
        profile = merged.pop('profile', None)
        engine = merged.pop('engine', 'experimental')
        report, out_v, out_t = repair.repair_mesh_from_arrays(
            verts, tris, tmpdir, mode=mode, profile=profile, engine=engine,
            **merged)
        out_v, out_t = repair.maybe_run_stage2(report, out_v, out_t, tmpdir)
        repair.enforce_reload_verdict(report, out_v, out_t)
        report = dict(report)
        report['_verts'] = out_v
        report['_tris'] = out_t
        return report

    def repair(self, ctx: RepairContext) -> MethodResult:
        """Execute the method on the provided context and return MethodResult."""
        import tempfile
        tmp = ctx.tmpdir or tempfile.mkdtemp()
        rep = self.run(ctx.verts, ctx.tris, tmp, ctx.params)
        out_v = rep.get('_verts')
        out_t = rep.get('_tris')
        ok = bool(rep.get('ok', True))
        err = rep.get('error')
        return MethodResult(ok=ok, verts=out_v, tris=out_t, report=rep, error=err)
