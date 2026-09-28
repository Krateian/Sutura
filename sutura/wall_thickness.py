# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Backward-compatibility shim for sutura.wall_thickness.

The implementation lives in ``sutura_engine.wall`` (F1 engine layout); this
module keeps the legacy top-level import path working.
"""
from sutura_engine import wall as _wall
from sutura_engine.wall import *  # noqa: F401,F403


def __getattr__(name):
    return getattr(_wall, name)
