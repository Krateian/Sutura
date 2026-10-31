#!/usr/bin/env python3
"""Step 1 policy gate: the policy-backed tier ORDER must equal the historical
literals, and the policy metadata (schema version + content hash) must exist.

This pins the data-like values that moved from ``pipeline/order.py`` into
``policy/default_policy.toml``; if the file drifts from the code's historical
behaviour this test fails before any repair run.

Usage: <venv>/bin/python tests/test_policy_order.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.pipeline.order as order  # noqa: E402
from sutura_engine.policy import CONTENT_HASH, ORDER, SCHEMA_VERSION  # noqa: E402

# The historical literals (P1b), pre-policy-file.
HIST_ORDER = ('local', 'graft', 'dressing', 'splint', 'coaptation', 'ftetwild')
HIST_METHOD_NUM = {'local': 2, 'graft': 13, 'dressing': 16, 'splint': 18,
                   'coaptation': 19, 'ftetwild': 7}
HIST_PRECEDENCE = ('ftetwild', 'cdt', 'graft', 'dressing', 'splint',
                   'coaptation', 'local')


def test_order_matches_historical_literals():
    assert order.DEEP_TIER_ORDER == HIST_ORDER, order.DEEP_TIER_ORDER
    assert order.DEEP_TIER_METHOD_NUM == HIST_METHOD_NUM
    assert order.FINAL_TIER_PRECEDENCE == HIST_PRECEDENCE
    assert order.GRAFT == 'graft' and order.FTETWILD == 'ftetwild'
    assert order.GRAFT_METHOD_NUM == 13 and order.FTETWILD_METHOD_NUM == 7


def test_policy_is_the_source():
    # The reader exposes exactly the policy data, no second copy.
    assert tuple(ORDER['deep_tier_order']) == HIST_ORDER
    assert dict(ORDER['deep_tier_method_num']) == HIST_METHOD_NUM
    assert isinstance(SCHEMA_VERSION, int) and SCHEMA_VERSION >= 1
    assert isinstance(CONTENT_HASH, str) and len(CONTENT_HASH) == 16


def test_method_num_and_tiebreaks():
    assert order.method_num('graft') == 13
    assert order.method_num('ftetwild') == 7
    assert order.graft_precedes_ftetwild('graft', 'ftetwild') is True
    assert order.graft_precedes_ftetwild('ftetwild', 'graft') is False
    assert order.graft_precedes_ftetwild('local', 'graft') is True
    # ftetwild_supersedes_graft: coverage failure always supersedes; otherwise
    # only when both deviations are known and fTetWild is lower.
    assert order.ftetwild_supersedes_graft(0.01, 0.02) is True
    assert order.ftetwild_supersedes_graft(0.02, 0.01) is False
    assert order.ftetwild_supersedes_graft(0.01, None) is False
    assert order.ftetwild_supersedes_graft(None, None) is False
    assert order.ftetwild_supersedes_graft(0.02, 0.01, coverage_failed=True) is True


if __name__ == '__main__':
    test_order_matches_historical_literals()
    test_policy_is_the_source()
    test_method_num_and_tiebreaks()
    print('policy order tests passed (schema_version=%d hash=%s)'
          % (SCHEMA_VERSION, CONTENT_HASH))
