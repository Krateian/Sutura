# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Single source of truth for the deep-repair TIER ORDER (P1b).

The last-resort tiers of the deep-repair ladder and the auto-escalation policy
used to restate their relative order (and the Graft / fTetWild method numbers)
as bare literals in several places. This module gathers them:

- ``DEEP_TIER_ORDER`` is the order in which the ladder tries the tiers that can
  produce the final mesh: local re-mesh, then Graft, then Dressing, then
  fTetWild. ``triage._auto`` must attempt Graft before fTetWild regardless of
  the ranked recommendation order (``graft_precedes_ftetwild`` /
  ``order_graft_before_ftetwild``).
- ``DEEP_TIER_METHOD_NUM`` maps each tier to its registry method number
  (``method_num``), used wherever a tier is looked up by number.
- ``FINAL_TIER_PRECEDENCE`` is the order in which ``_deep_repair_offer`` names
  the reported ``final_tier``; it is the ladder's precedence list, not the run
  order.
- ``ftetwild_supersedes_graft`` is the one healthy-deviation tie-break used when
  a Graft candidate and an fTetWild candidate both close: fTetWild wins only
  when both deviations are known and lower, or on an input-coverage failure.
  The coverage argument is used by the ladder only; the triage auto paths have
  no coverage concept and always keep Graft when a deviation is unknown.

This module is a stdlib-only leaf (no numpy / pymeshlab / third-party import)
so ``triage`` can import it without pulling anything in.
"""

# Run order of the deep-repair tiers (local re-mesh first, fTetWild last;
# Splint sits after Dressing; Coaptation after Splint and before fTetWild).
DEEP_TIER_ORDER = ('local', 'graft', 'dressing', 'splint', 'coaptation',
                   'ftetwild')

# Registry method number for each tier.
DEEP_TIER_METHOD_NUM = {
    'local': 2,       # Local Mend
    'graft': 13,      # Graft
    'dressing': 16,   # Dressing
    'splint': 18,     # Splint
    'coaptation': 19,  # Coaptation
    'ftetwild': 7,    # fTetWild
}

# Precedence used by ``_deep_repair_offer`` when naming the reported final
# tier. Kept separate from DEEP_TIER_ORDER because the reporting precedence is
# not the run order (fTetWild is named first, not last).
FINAL_TIER_PRECEDENCE = ('ftetwild', 'graft', 'dressing', 'splint',
                         'coaptation', 'local')

# Convenience aliases for the two tiers whose relative order the auto policy
# guarantees explicitly.
GRAFT = 'graft'
FTETWILD = 'ftetwild'
GRAFT_METHOD_NUM = DEEP_TIER_METHOD_NUM[GRAFT]
FTETWILD_METHOD_NUM = DEEP_TIER_METHOD_NUM[FTETWILD]


def method_num(tier):
    """Registry method number of ``tier`` (one of DEEP_TIER_ORDER)."""
    return DEEP_TIER_METHOD_NUM[tier]


def graft_precedes_ftetwild(a, b):
    """Whether tier ``a`` must be attempted before tier ``b``.

    Only the Graft-before-fTetWild guarantee is meaningful; for any other pair
    ``a`` precedes ``b`` iff it comes first in DEEP_TIER_ORDER.
    """
    if a == GRAFT and b == FTETWILD:
        return True
    if a == FTETWILD and b == GRAFT:
        return False
    return DEEP_TIER_ORDER.index(a) < DEEP_TIER_ORDER.index(b)


def order_graft_before_ftetwild(recs):
    """Return ``recs`` re-ordered so a Graft recommendation precedes fTetWild.

    ``recs`` is a list of recommendation namedtuples with a ``num`` field
    (``sutura_engine.methods.Recommendation``). The list is copied; the
    original is not mutated. When either method is absent, or Graft already
    precedes fTetWild, the input order is preserved.
    """
    nums = [r.num for r in recs]
    if (GRAFT_METHOD_NUM in nums and FTETWILD_METHOD_NUM in nums
            and nums.index(GRAFT_METHOD_NUM) > nums.index(FTETWILD_METHOD_NUM)):
        out = list(recs)
        out.insert(nums.index(FTETWILD_METHOD_NUM),
                   out.pop(nums.index(GRAFT_METHOD_NUM)))
        return out
    return list(recs)


def ftetwild_supersedes_graft(ft_dev, graft_dev, coverage_failed=False):
    """Whether fTetWild should supersede a held-aside Graft candidate.

    The single tie-break rule (ladder semantics): an input-coverage failure
    always lets fTetWild win; otherwise fTetWild wins only when BOTH healthy
    deviations are known and ``ft_dev < graft_dev``. When either deviation is
    ``None``, Graft is kept. Both the ladder and ``triage._auto`` pass the
    coverage flag, so the rule is shared.
    """
    if coverage_failed:
        return True
    return (ft_dev is not None and graft_dev is not None
            and ft_dev < graft_dev)
