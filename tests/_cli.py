# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
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

**Isolation.** ``repair.main()`` calls ``enable_crash_logging()`` before doing
any work (``repair.py:1399-1406``), which creates ``<data>/crash.log`` and
enables ``faulthandler`` into it. A child that inherits the real environment
therefore writes that file into the tester's ``~/.local/share/sutura`` -- which
is exactly what happened, and why a bare test run left a stray ``crash.log``
behind. :func:`cli_env` builds an environment that points every user path at a
throwaway directory; use it for **every** child process.
"""
import os
import shlex
import sys
import tempfile

__all__ = ['REPO', 'cli_command', 'cli_env', 'scratch_home']

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# One throwaway tree per process: cheap, and shared by every call so a suite's
# children all see the same config/cache/data.
_SCRATCH = None


def scratch_home():
    """The throwaway HOME for this test process, created on first use."""
    global _SCRATCH
    if _SCRATCH is None:
        _SCRATCH = tempfile.mkdtemp(prefix='sutura-test-home-')
    return _SCRATCH


def cli_env(extra=None):
    """An environment for a CLI child that cannot touch the real user paths.

    Sets ``HOME``, ``XDG_CONFIG_HOME``, ``XDG_CACHE_HOME`` and
    ``SUTURA_DATA_DIR`` inside :func:`scratch_home`, so ``crash.log``,
    ``history.jsonl``, the analysis cache and the policy/config files all land
    in the temp tree. ``SUTURA_NO_CACHE`` is set because a shared cache across
    tests makes a cold/warm distinction meaningless.
    """
    home = scratch_home()
    env = dict(os.environ)
    env.update({
        'HOME': home,
        'XDG_CONFIG_HOME': os.path.join(home, 'config'),
        'XDG_CACHE_HOME': os.path.join(home, 'cache'),
        'SUTURA_DATA_DIR': os.path.join(home, 'data'),
        'SUTURA_NO_CACHE': '1',
    })
    env.pop('SUTURA_POLICY', None)
    env.pop('SUTURA_POLICY_USER', None)
    env.pop('SUTURA_INTENSITY', None)
    if extra:
        env.update(extra)
    return env


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
