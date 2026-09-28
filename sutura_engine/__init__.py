# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Repository-root bootstrap for ``import sutura_engine`` (source checkout only).

The engine package lives at ``sutura/sutura_engine``. Installed and AppImage
layouts copy it to a top-level ``sutura_engine`` package, so this bootstrap is
never used there. In a source checkout it makes ``import sutura_engine`` (and
``import sutura_engine.<submodule>``) work from the repository root without the
former root symlink, which was removed because symlinks are not portable across
AppImage, zip archives and Windows.

It only adds ``sutura/`` to ``sys.path`` and points ``__path__`` at the real
package directory (a package with an explicit ``__path__`` that is not the
directory it lives in is left untouched by PyInstaller's fallback-py-module
conversion), so the package's own relative imports keep resolving without
fabricating or replacing any module objects.
"""
import os as _os
import sys as _sys

_root = _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__)))
_engine_dir = _os.path.join(_root, 'sutura', 'sutura_engine')
_sutura_dir = _os.path.join(_root, 'sutura')
if _sutura_dir not in _sys.path:
    _sys.path.insert(0, _sutura_dir)

__path__ = [_engine_dir]

from sutura.sutura_engine import *  # noqa: E402,F401,F403
from sutura.sutura_engine import __all__  # noqa: E402,F401
