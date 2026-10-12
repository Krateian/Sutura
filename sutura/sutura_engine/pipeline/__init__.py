# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Sutura repair pipeline package (P1a mechanical split of sutura/repair.py).

Modules are moved out of the former monolith by symbol, byte-for-byte, with no
behaviour change. ``repair.py`` remains a thin compatibility shim that imports
and re-exports these symbols. Low-level modules import only stdlib,
third-party and other pipeline modules -- never ``repair``.
"""
