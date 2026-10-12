# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Sutura method health: one central error-isolation wrapper and availability.

This is a LEAF module (stdlib only, no numpy / pymeshlab / sutura-engine
imports at module scope) so that BOTH ``sutura/repair.py`` and
``sutura/sutura_engine/triage.py`` can import it without closing an import
cycle.  It never imports ``repair``.

Public API (the CLI ``--self-check`` and any other caller reuse exactly these,
so there is only ONE availability checker and ONE error type in the project):

  ``Status``
      Enum of a method's state: ``OK`` / ``SKIP`` / ``FAIL``.
  ``MethodError``
      Frozen dataclass ``{method, stage, kind, message}`` for one failed run.
  ``KIND_IMPORT`` / ``KIND_MISSING_SYMBOL`` / ``KIND_TIMEOUT`` / ``KIND_EXCEPTION``
      The ``kind`` values ``MethodError`` uses.
  ``check_availability() -> dict[str, Status]``
      Per registered method id, its availability state.  A missing optional
      addon (e.g. fTetWild) is ``SKIP``; a broken availability check is
      ``FAIL``; everything present is ``OK``.
  ``check_availability_detail() -> dict[str, (Status, reason|None)]``
      Same, with the human-readable reason for ``SKIP``/``FAIL``.
  ``record_error(method, stage, exc) -> MethodError | None``
      Classify ``exc`` and append one ``MethodError`` to the active collector.
      The whole pipeline uses this; it is the single recording point.
  ``tier_guard(method, stage)``
      Context manager: runs a tier/method body, catches its exception (except
      ``KeyboardInterrupt``/``SystemExit``), records a ``MethodError`` and
      SWALLOWS it so the caller's existing "not adopted" fallback runs.  The
      guard exposes the swallowed exception as ``.error`` (``None`` on
      success) for the caller to translate into its own report fields.
  ``guard(method, stage, fn, *args, **kwargs) -> (ok, value)``
      One-shot call form of ``tier_guard`` for a single risky call; on failure
      returns ``(False, exc)`` after recording.
  ``current() -> _Collector``
      The active collector; ``collector.errors`` is the ``MethodError`` list.
  ``begin()`` / ``end()``
      Open/close a per-run collection scope (used at the file boundary).
  ``build_report(collector) -> dict``
      The JSON ``method_health`` section: availability overlaid with the
      run's runtime errors, plus the error list.

Environment: ``SUTURA_METHOD_HEALTH=0`` disables RECORDING only (the guards
still isolate exceptions exactly as before); this is the parity switch the
regression test uses to prove recording never changes a repair result.

Deliberately NOT routed through ``tier_guard`` (they are not a tier/method
run, and wrapping them would change bookkeeping the top level relies on):

  * ``repair.py`` Flap/Tamponade FINAL safety nets (the post-file re-runs):
    their try/except chooses the surviving candidate; it must stay local.
  * ``repair.py:p_weld_final`` -- a save/reload heal pass, not a method.
  * ``repair.py:process_file`` -- the outermost per-file catch; it is the
    caller that turns any escaped exception into ``{'error': ...}`` + exit 1.
  * ``triage._analyze_objects`` and the post-adoption score block -- optional
    diagnostics, never a repair method.
  * ``RepairMethod.available`` -- already never raises; consumed directly by
    ``check_availability``.
"""
import contextlib
import contextvars
import enum
import os
from dataclasses import asdict, dataclass
from typing import Any, Dict, List, Optional, Tuple

KIND_IMPORT = 'import'
KIND_MISSING_SYMBOL = 'missing_symbol'
KIND_TIMEOUT = 'timeout'
KIND_EXCEPTION = 'exception'

_ENV_VAR = 'SUTURA_METHOD_HEALTH'


class Status(str, enum.Enum):
    """Availability / outcome state of one repair method."""
    OK = 'ok'
    SKIP = 'skip'      # optional addon absent (not a failure)
    FAIL = 'fail'      # availability check raised, or the method errored at run time


@dataclass(frozen=True)
class MethodError:
    """One failed method/tier run, isolated by the central wrapper."""
    method: str
    stage: str
    kind: str
    message: str

    def to_dict(self) -> Dict[str, str]:
        return asdict(self)


def enabled() -> bool:
    """False only when ``SUTURA_METHOD_HEALTH`` explicitly disables recording."""
    return (os.environ.get(_ENV_VAR, '1').strip().lower()
            not in ('0', 'false', 'no', 'off'))


def classify_exception(exc: BaseException) -> str:
    """Map an exception to a ``KIND_*`` value."""
    if isinstance(exc, (ImportError, ModuleNotFoundError)):
        return KIND_IMPORT
    if isinstance(exc, AttributeError):
        return KIND_MISSING_SYMBOL
    if isinstance(exc, TimeoutError):
        return KIND_TIMEOUT
    name = type(exc).__name__
    if name in ('TimeoutExpired', 'PanicException'):
        return KIND_TIMEOUT if name == 'TimeoutExpired' else KIND_EXCEPTION
    return KIND_EXCEPTION


class _Collector:
    """Per-run list of ``MethodError`` records (thread/context local)."""
    __slots__ = ('errors',)

    def __init__(self) -> None:
        self.errors: List[MethodError] = []

    def to_dicts(self) -> List[Dict[str, str]]:
        return [e.to_dict() for e in self.errors]


_CURRENT: 'contextvars.ContextVar[Optional[_Collector]]' = contextvars.ContextVar(
    'sutura_health_current', default=None)
_SCOPED: 'contextvars.ContextVar[bool]' = contextvars.ContextVar(
    'sutura_health_scoped', default=False)


def current() -> _Collector:
    """The active collector, creating one when no scope is open."""
    c = _CURRENT.get()
    if c is None:
        c = _Collector()
        _CURRENT.set(c)
    return c


def begin() -> _Collector:
    """Open a per-run collection scope.  Pairs with :func:`end`."""
    c = _Collector()
    _CURRENT.set(c)
    _SCOPED.set(True)
    return c


def end() -> None:
    """Close the scope opened by :func:`begin`."""
    _CURRENT.set(None)
    _SCOPED.set(False)


def reset_if_unscoped() -> None:
    """Start a fresh collector for a direct library call with no open scope."""
    if not _SCOPED.get():
        _CURRENT.set(_Collector())


@contextlib.contextmanager
def collecting():
    """Nested-safe collection scope; reuses the active collector when present."""
    existing = _CURRENT.get()
    if existing is not None:
        yield existing
        return
    c = _Collector()
    tok_c = _CURRENT.set(c)
    tok_s = _SCOPED.set(True)
    try:
        yield c
    finally:
        _CURRENT.reset(tok_c)
        _SCOPED.reset(tok_s)


def record_error(method: str, stage: str, exc: BaseException) -> Optional[MethodError]:
    """Classify ``exc`` and record it in the active collector (unless disabled)."""
    if not enabled():
        return None
    kind = classify_exception(exc)
    message = '%s: %s' % (type(exc).__name__, exc)
    err = MethodError(str(method), str(stage), kind, message)
    current().errors.append(err)
    return err


def record_missing_symbol(method: str, stage: str, symbol: str) -> Optional[MethodError]:
    """Record a Rust symbol that was detected absent via ``getattr``/``hasattr``."""
    if not enabled():
        return None
    err = MethodError(str(method), str(stage), KIND_MISSING_SYMBOL,
                      'missing symbol: %s' % symbol)
    current().errors.append(err)
    return err


class _TierGuard:
    """Context manager that catches a tier body, records it and swallows it."""
    __slots__ = ('method', 'stage', 'error')

    def __init__(self, method: str, stage: str) -> None:
        self.method = str(method)
        self.stage = str(stage)
        self.error: Optional[BaseException] = None

    def __enter__(self) -> '_TierGuard':
        return self

    def __exit__(self, exc_type, exc, tb) -> bool:
        if exc is None:
            return False
        if isinstance(exc, (KeyboardInterrupt, SystemExit)):
            return False  # never swallow an operator interrupt / clean exit
        self.error = exc
        record_error(self.method, self.stage, exc)
        return True


def tier_guard(method: str, stage: str) -> _TierGuard:
    """Central wrapper for one tier/method run (see the module docstring)."""
    return _TierGuard(method, stage)


def guard(method: str, stage: str, fn, *args, **kwargs):
    """Run a single risky call under the central wrapper.

    Returns ``(True, value)`` on success or ``(False, exc)`` after recording
    the failure.  ``KeyboardInterrupt``/``SystemExit`` always propagate.
    """
    try:
        return True, fn(*args, **kwargs)
    except (KeyboardInterrupt, SystemExit):
        raise
    except BaseException as exc:  # noqa: BLE001 - the wrapper is the sole catch
        record_error(method, stage, exc)
        return False, exc


def _registered_methods() -> List[Any]:
    """Every registered method (lazy import; never raises)."""
    try:
        from sutura_engine import methods as _methods
        return list(_methods.all_methods())
    except Exception:  # noqa: BLE001 - a missing registry is not fatal
        return []


def check_availability_detail() -> Dict[str, Tuple[Status, Optional[str]]]:
    """Per method id: ``(Status, reason|None)``.  The single availability checker."""
    out: Dict[str, Tuple[Status, Optional[str]]] = {}
    for m in _registered_methods():
        mid = getattr(m, 'id', None) or str(getattr(m, 'num', '?'))
        try:
            ok, reason = m.available()
        except BaseException as exc:  # noqa: BLE001 - report, never crash
            out[mid] = (Status.FAIL, '%s: %s' % (type(exc).__name__, exc))
            continue
        out[mid] = (Status.OK, None) if ok else (Status.SKIP, reason)
    return out


def check_availability() -> Dict[str, Status]:
    """Per registered method id, its :class:`Status`.  Reused by ``--self-check``."""
    return {mid: st for mid, (st, _r) in check_availability_detail().items()}


def build_report(collector: Optional[_Collector] = None,
                 availability: Optional[Dict[str, Tuple[Status, Optional[str]]]] = None) -> Dict[str, Any]:
    """The JSON ``method_health`` section: availability + this run's errors."""
    collector = collector if collector is not None else current()
    avail = availability if availability is not None else check_availability_detail()
    errors = list(collector.errors)
    errored = {e.method for e in errors}
    methods: Dict[str, Dict[str, Any]] = {}
    for mid, (state, reason) in avail.items():
        st = Status.FAIL if mid in errored else state
        methods[mid] = {'status': st.value, 'reason': reason}
    ok = sum(1 for v in methods.values() if v['status'] == Status.OK.value)
    failed = sum(1 for v in methods.values() if v['status'] == Status.FAIL.value)
    skipped = sum(1 for v in methods.values() if v['status'] == Status.SKIP.value)
    return {
        'total': len(methods),
        'ok': ok,
        'failed': failed,
        'skipped': skipped,
        'methods': methods,
        'errors': [e.to_dict() for e in errors],
    }


def human_line(report: Dict[str, Any]) -> str:
    """``Methods: N/M OK (X failed, Y skipped)`` with the tail only when non-zero."""
    total = report.get('total', 0)
    ok = report.get('ok', 0)
    failed = report.get('failed', 0)
    skipped = report.get('skipped', 0)
    line = 'Methods: %d/%d OK' % (ok, total)
    extra = []
    if failed:
        extra.append('%d failed' % failed)
    if skipped:
        extra.append('%d skipped' % skipped)
    if extra:
        line += ' (%s)' % ', '.join(extra)
    return line
