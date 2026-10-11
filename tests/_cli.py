# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Shared helper resolving the Sutura CLI entry point for test harnesses.

Test suites must exercise the code in THIS checkout, never a stale globally
installed ``sutura`` (an old global install silently masks a regression in the
working tree). The command is therefore resolved as:

1. an explicit ``SUTURA`` environment override -- a launcher/executable path or
   a full command line -- when set;
2. otherwise the in-repo entry point: this interpreter running
   ``<repo>/sutura/repair.py``.

CI jobs that intentionally exercise an installed CLI (or a wrapper around the
in-repo entry point) keep that behaviour by setting ``SUTURA`` explicitly, e.g.
``.github/workflows/ci.yml`` (``SUTURA: /tmp/sutura-wrap``). The in-repo default
requires the suite to be run with an interpreter that can satisfy
``repair.py``'s imports (the venv with pymeshlab, e.g. the CI ``./venv`` or a
worktree ``.venv``).
"""
import os
import shlex
import sys

__all__ = ['REPO', 'cli_command']

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def cli_command(args=None):
    """Return the argv for invoking the Sutura CLI.

    ``args`` is an optional sequence appended after the resolved command. The
    base command is the ``SUTURA`` override when set, else
    ``[sys.executable, <repo>/sutura/repair.py]``.
    """
    override = os.environ.get('SUTURA')
    if override:
        base = shlex.split(override)
    else:
        base = [sys.executable, os.path.join(REPO, 'sutura', 'repair.py')]
    return base + list(args or [])
