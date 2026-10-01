# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Shared Qt test-environment helper (test-side only).

On a macOS conda install a second Qt (qt-main / Qt5, pulled in by the
conda-forge pymeshlab) drops a ``bin/qt.conf`` into the environment prefix. A
plain interpreter then resolves ``QLibraryInfo``'s plugin search path to the
Qt5 ``$PREFIX/plugins`` directory instead of PySide6's own Qt6 plugins, and the
GUI suites abort with "Could not find the Qt platform plugin offscreen" (or
"cocoa" on a real display).

This helper derives PySide6's own plugin directory from the installed package
location -- independent of the conda prefix and the qt.conf hijack -- and
exports it as ``QT_PLUGIN_PATH`` when that variable is unset. It is imported by
the GUI-constructing suites before PySide6 is used, so the value is also
inherited by the GUI subprocesses they spawn.

Application startup is not affected: the ``sutura-gui`` launcher computes the
same directory through ``conda run`` (see install-macos.sh), which sets
``CONDA_PREFIX`` and therefore already resolves to PySide6's own plugins.
"""
import importlib.util
import os

__all__ = ['qt_plugins_path', 'ensure_qt_plugins']


def qt_plugins_path():
    """Absolute path to PySide6's bundled Qt6 plugin directory, or None."""
    spec = importlib.util.find_spec('PySide6')
    if spec is None or not spec.origin:
        return None
    package_dir = os.path.dirname(os.path.abspath(spec.origin))
    return os.path.join(package_dir, 'Qt', 'plugins')


def ensure_qt_plugins():
    """Set ``QT_PLUGIN_PATH`` to PySide6's own Qt6 plugins when unset.

    Returns the path that is in effect afterwards (the existing value when one
    was already set, PySide6's directory when it was set here, ``None`` when
    PySide6 is not installed).
    """
    existing = os.environ.get('QT_PLUGIN_PATH')
    if existing:
        return existing
    path = qt_plugins_path()
    if path and os.path.isdir(path):
        os.environ['QT_PLUGIN_PATH'] = path
    return path
