# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Backward-compatibility shim for sutura.mirror_repair.

The implementation lives in ``sutura_engine.mirror`` (F1 engine layout); this
module keeps the legacy top-level import path working.
"""
from sutura_engine import mirror as _mirror
from sutura_engine.mirror import *  # noqa: F401,F403


def __getattr__(name):
    return getattr(_mirror, name)
