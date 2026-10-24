# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Deterministic, countable work budgets for repair tiers.

A work budget is a pure function of the input and the intensity preset: the
tier spends whole-number work units (loop DP operations, grid cells, candidate
faces, ...) and stops when the next step would exceed the limit.  No wall
clock is read here, so the produced mesh is identical on every machine and
under any CPU load.

Wall-clock ``seconds`` fields remain in the tier reports as measurements.  A
decision must never read them; it reads ``work_units``/``work_budget`` instead.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class WorkBudget:
    """A monotonic work counter with an optional hard limit.

    ``limit <= 0`` disables the consumer.  ``None`` means unlimited (the
    caller still records the spent units).
    """

    limit: int | None
    unit: str = 'unit'
    spent: int = 0

    def __post_init__(self):
        if self.limit is not None:
            self.limit = int(self.limit)

    @property
    def remaining(self) -> int | None:
        if self.limit is None:
            return None
        return max(int(self.limit) - int(self.spent), 0)

    @property
    def exhausted(self) -> bool:
        return self.limit is not None and self.spent >= int(self.limit)

    def fits(self, cost: int) -> bool:
        """Whether ``cost`` more units fit in the budget."""
        if self.limit is None:
            return True
        return int(self.spent) + int(cost) <= int(self.limit)

    def consume(self, cost: int) -> bool:
        """Add ``cost`` units; return False when the limit was exceeded."""
        self.spent += int(cost)
        return self.limit is None or self.spent <= int(self.limit)

    def stop_before(self, cost: int) -> bool:
        """Whether a step costing ``cost`` must be skipped."""
        return not self.fits(cost)

    def as_report(self) -> dict:
        return {
            'work_units': int(self.spent),
            'work_budget': (None if self.limit is None else int(self.limit)),
            'work_unit': self.unit,
            'budget_limited_by': ('work' if self.exhausted else None),
        }


def work_units(limit, unit='unit'):
    """Convenience constructor used by the tier policy tables."""
    return WorkBudget(limit=limit, unit=unit)
