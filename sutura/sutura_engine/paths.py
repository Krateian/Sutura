# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""sutura_engine.paths - runtime user-data location and one-time migration.

User data written at runtime (the anonymous usage history and the learning-
triage counters) lives in a dedicated ``data/`` directory, separate from the
application code. This keeps it out of the way of the self-updater (which
replaces ``APP_DIR`` wholesale) and of uninstall, and lets the AppImage - whose
``SUTURA_DIR`` is a read-only mount - persist it at last.

Location: ``SUTURA_DATA_DIR`` if set and non-empty, otherwise
``~/.local/share/sutura/data``. ``SUTURA_DIR`` no longer influences the data
location at all; it only names the application/code directory. The legacy
files under ``~/.local/share/sutura`` are moved into the data dir once, on
first use, idempotently and without ever deleting data on failure.

stdlib only (no numpy/pymeshlab), safe to import from anywhere.
"""
import os
import shutil

DATA_ENV = 'SUTURA_DATA_DIR'
DEFAULT_DATA_DIR = os.path.expanduser('~/.local/share/sutura/data')

# Legacy location: before this module existed, history.jsonl and
# triage_learning.json were written directly under the install/code directory.
LEGACY_DIR = os.path.expanduser('~/.local/share/sutura')
LEGACY_DATA_FILES = ('history.jsonl', 'triage_learning.json')


def data_dir():
    """The runtime user-data directory (never the code directory)."""
    override = os.environ.get(DATA_ENV)
    if override:
        return os.path.expanduser(override)
    return DEFAULT_DATA_DIR


def data_file(name):
    """Absolute path of ``name`` inside the data directory."""
    return os.path.join(data_dir(), name)


def _is_explicit_override(dest):
    """True when the data dir came from SUTURA_DATA_DIR and differs from the
    default. Such a caller (tests, power users) owns the location, so the
    automatic migration must not read the real legacy directory."""
    return os.path.abspath(dest) != os.path.abspath(DEFAULT_DATA_DIR)


def migrate_data_dir(legacy_dir=None, names=LEGACY_DATA_FILES, dest=None):
    """Move legacy data files into the data directory. Never raises.

    For each name: skip when the source is missing or the destination already
    exists (never clobber), otherwise create the destination and move it with
    an atomic same-filesystem rename; a cross-device/permission failure falls
    back to a copy that leaves the source in place. Returns a dict with
    ``data_dir``, ``moved``, ``skipped`` and ``errors``.
    """
    legacy = legacy_dir or LEGACY_DIR
    target = dest or data_dir()
    info = {'data_dir': target, 'moved': [], 'skipped': [], 'errors': []}
    if os.path.abspath(legacy) == os.path.abspath(target):
        return info
    for name in names:
        src = os.path.join(legacy, name)
        dst = os.path.join(target, name)
        try:
            if not os.path.exists(src):
                continue
            if os.path.exists(dst):
                info['skipped'].append(name)
                continue
            os.makedirs(target, exist_ok=True)
            try:
                os.replace(src, dst)
            except OSError:
                # cross-device or permission: copy, leave the source in place
                shutil.copy2(src, dst)
            info['moved'].append(name)
        except Exception as exc:  # never let migration break a repair
            info['errors'].append('%s: %s' % (name, exc))
    return info


def ensure_data_dir(legacy_dir=None):
    """Create the data directory and run the one-time migration. Idempotent,
    cheap (existence checks) and never raises. Returns the migration info."""
    target = data_dir()
    info = {'data_dir': target, 'moved': [], 'skipped': [], 'errors': []}
    try:
        os.makedirs(target, exist_ok=True)
    except OSError as exc:
        info['errors'].append('data dir: %s' % exc)
        return info
    if _is_explicit_override(target):
        return info
    moved = migrate_data_dir(legacy_dir=legacy_dir, dest=target)
    info['moved'] = moved['moved']
    info['skipped'] = moved['skipped']
    info['errors'] += moved['errors']
    return info
