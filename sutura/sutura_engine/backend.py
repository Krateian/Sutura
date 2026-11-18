# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
"""Product backend selection for the P10 transition.

``SUTURA_BACKEND=rust`` (the default) makes the Python service use the Rust
pipeline/cores and the entry points prefer the Rust CLI/GUI.  The one-release
fallback is ``SUTURA_BACKEND=python`` (or ``--backend python`` on the Python
CLI), which keeps the legacy Python pipeline and prints a deprecation note.

The per-engine switches (``SUTURA_PIPELINE_ENGINE`` and friends) remain
available under the Python backend.  Under the Rust backend they are forced to
``rust`` and ignored, as the product backend wins; the original values are
restored when the Python backend is selected, so an explicit per-engine
setting made before the process started is not lost.
"""
import os
import shutil

BACKEND_ENV = 'SUTURA_BACKEND'
RUST = 'rust'
PYTHON = 'python'
DEFAULT = RUST

# Transition engines that must follow the product backend.  I/O, report and
# config are not here: they already default to Rust and are byte-identical on
# the Python backend, so the legacy pipeline keeps its existing defaults.
_ENGINE_ENVS = (
    'SUTURA_PIPELINE_ENGINE',
    'SUTURA_TIER_ENGINE',
    'SUTURA_STAGE_ENGINE',
    'SUTURA_STAGE2_ENGINE',
    'SUTURA_PWELD_ENGINE',
    'SUTURA_CLOSING_ENGINE',
    'SUTURA_GRAFT_ENGINE',
)

_ORIGINAL = None
_SELECTED = None
_NOTE_PRINTED = False
_WARNED_BRIDGE = False

_DEPRECATION_NOTE = (
    'note: the Python backend is deprecated and will be removed in the next '
    'release.\n'
)


def resolve(value=None):
    """Return the selected backend: CLI value > env > default (Rust).

    An unknown CLI value is rejected by argparse; an unknown env value falls
    back to the default with no warning (the same forgiving rule the existing
    per-engine switches use).
    """
    if _SELECTED is not None and value is None:
        return _SELECTED
    raw = value if value is not None else os.environ.get(BACKEND_ENV, DEFAULT)
    text = str(raw).strip().lower()
    return text if text in (RUST, PYTHON) else DEFAULT


def apply_engine_defaults(value=None):
    """Pin the transition engines to the product backend.

    The first call snapshots the original environment.  Selecting Python
    restores it (so explicit per-engine switches keep working); selecting Rust
    forces every transition engine to Rust.
    """
    global _ORIGINAL, _SELECTED
    chosen = resolve(value)
    if _ORIGINAL is None:
        _ORIGINAL = {name: os.environ.get(name) for name in _ENGINE_ENVS}
    _SELECTED = chosen
    os.environ[BACKEND_ENV] = chosen
    if chosen == PYTHON:
        for name, original in _ORIGINAL.items():
            if original is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = original
        return chosen
    for name in _ENGINE_ENVS:
        os.environ[name] = RUST
    _warn_missing_bridge()
    return chosen


def _warn_missing_bridge():
    """One-line warning when the Rust core was selected but is not installed.

    A Python-only install (only ``sutura_geom``) has no ``sutura_core_py``; the
    transition engines then fall back to Python.  Say so once instead of
    silently ignoring the product default.
    """
    global _WARNED_BRIDGE
    if _WARNED_BRIDGE:
        return
    _WARNED_BRIDGE = True
    import importlib.util
    try:
        if importlib.util.find_spec('sutura_core_py') is not None:
            return
    except Exception:  # noqa: BLE001 - a broken path must not break startup
        pass
    import sys
    sys.stderr.write(
        'sutura: Rust backend selected but sutura_core_py is not installed; '
        'using the Python fallback for this release.\n')
    sys.stderr.flush()


def note_deprecated(value=None):
    """Print the one-line Python-backend deprecation note once per process."""
    global _NOTE_PRINTED
    if _NOTE_PRINTED or resolve(value) != PYTHON:
        return
    _NOTE_PRINTED = True
    import sys
    sys.stderr.write(_DEPRECATION_NOTE)
    sys.stderr.flush()


def is_python(value=None):
    return resolve(value) == PYTHON


def _candidate_paths(env_name):
    seen = set()
    out = []
    for raw in (os.environ.get(env_name),):
        if raw:
            out.append(raw)
    app_dir = os.environ.get(
        'SUTURA_APP_DIR', os.path.expanduser('~/.local/share/sutura'))
    out += [
        os.path.join(app_dir, 'bin', 'sutura'),
        os.path.expanduser('~/Applications/Sutura.app/Contents/MacOS/sutura'),
        '/Applications/Sutura.app/Contents/MacOS/sutura',
    ]
    which = shutil.which('sutura')
    if which:
        out.append(which)
    result = []
    for path in out:
        if path and path not in seen:
            seen.add(path)
            result.append(path)
    return result


def find_rust_cli():
    """Best-effort Rust CLI path for Python-side launchers, or ``None``."""
    for path in _candidate_paths('SUTURA_RUST_CLI'):
        if os.path.isfile(path) and os.access(path, os.X_OK):
            return path
    return None


def find_rust_gui():
    """Best-effort Rust GUI path for Python-side launchers, or ``None``."""
    app_dir = os.environ.get(
        'SUTURA_APP_DIR', os.path.expanduser('~/.local/share/sutura'))
    candidates = [
        os.environ.get('SUTURA_RUST_GUI'),
        os.path.join(app_dir, 'bin', 'sutura-gui'),
        os.path.expanduser('~/Applications/Sutura.app/Contents/MacOS/sutura-gui'),
        '/Applications/Sutura.app/Contents/MacOS/sutura-gui',
    ]
    for path in candidates:
        if path and os.path.isfile(path) and os.access(path, os.X_OK):
            return path
    return None
