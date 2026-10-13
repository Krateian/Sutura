# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Vulture whitelist: known false positives only.

Passed to vulture as an extra positional file. Vulture parses it and treats
every name referenced here as used; it is never imported or executed, so the
bare names below deliberately have no definitions.

Only framework-dispatched or protocol-mandated names belong here:

* Qt event handlers -- called by the Qt framework, never by name.
* PEP 562 module-level ``__getattr__`` shims -- the legacy import paths.
* ``__defaults__`` -- namedtuple/dataclass backward-compatibility hook.
* ``exc_type`` -- the ``__exit__`` context-manager protocol parameter.

Anything else that looks unused is left for the vulture + coverage
cross-check (scripts/anatomy.sh, step 4); it is not hidden here.
"""

# Qt event handlers (dispatched by the Qt framework).
paintEvent  # noqa: F821
mouseMoveEvent  # noqa: F821
wheelEvent  # noqa: F821
dragEnterEvent  # noqa: F821
dragMoveEvent  # noqa: F821
dropEvent  # noqa: F821

# PEP 562 module-level __getattr__ shims (legacy import paths).
__getattr__  # noqa: F821

# namedtuple / dataclass compatibility hook.
__defaults__  # noqa: F821

# contextlib __exit__ protocol parameter.
exc_type  # noqa: F821
