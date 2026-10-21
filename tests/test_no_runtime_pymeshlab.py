# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""CI guard: prevent new PyMeshLab runtime imports in sutura/.

Scans all Python modules under sutura/ (excluding tests/ and scripts/) to
ensure no runtime code imports pymeshlab unless explicitly allowlisted during
the phased migration (W1-W5).

As work packages complete, allowlist entries must be removed until the
allowlist is completely empty. Stale allowlist entries (where the file is
already free of pymeshlab imports) trigger a failure to enforce ratchet progress.
"""
import ast
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')

# Relative path from sutura/ -> (owning_work_package, expected_sites, description)
RUNTIME_PYMESHLAB_ALLOWLIST = {}


def _find_runtime_pymeshlab_imports(base_dir=SUTURA):
    """Scan base_dir for all ast.Import and ast.ImportFrom targeting pymeshlab."""
    found = {}
    for root, _, files in os.walk(base_dir):
        for f in files:
            if not f.endswith('.py'):
                continue
            abs_path = os.path.join(root, f)
            rel_path = os.path.relpath(abs_path, base_dir)
            with open(abs_path, 'r', encoding='utf-8') as fh:
                try:
                    tree = ast.parse(fh.read(), filename=abs_path)
                except SyntaxError as e:
                    raise AssertionError(f"Syntax error parsing {abs_path}: {e}")
            for node in ast.walk(tree):
                if isinstance(node, ast.Import):
                    for alias in node.names:
                        if alias.name == 'pymeshlab' or alias.name.startswith('pymeshlab.'):
                            found.setdefault(rel_path, []).append(node.lineno)
                elif isinstance(node, ast.ImportFrom):
                    if node.module and (node.module == 'pymeshlab' or node.module.startswith('pymeshlab.')):
                        found.setdefault(rel_path, []).append(node.lineno)
    return found


def test_no_unauthorized_runtime_pymeshlab_imports():
    """Fail if any file under sutura/ imports pymeshlab without being allowlisted."""
    found = _find_runtime_pymeshlab_imports()
    found_paths = set(found.keys())
    allowlisted_paths = set(RUNTIME_PYMESHLAB_ALLOWLIST.keys())

    # 1. Check for unauthorized imports (ratchet guard)
    unauthorized = found_paths - allowlisted_paths
    assert not unauthorized, (
        f"Found {len(unauthorized)} unauthorized runtime file(s) importing pymeshlab under sutura/:\n"
        + "\n".join(f"  - {p} (lines {found[p]})" for p in sorted(unauthorized))
        + "\nRuntime code must be native or explicitly registered in RUNTIME_PYMESHLAB_ALLOWLIST."
    )

    # 2. Check for stale allowlist entries (ratchet advancement)
    stale = allowlisted_paths - found_paths
    assert not stale, (
        f"Allowlist contains {len(stale)} stale entry/entries that no longer import pymeshlab:\n"
        + "\n".join(f"  - {p} (owner: {RUNTIME_PYMESHLAB_ALLOWLIST[p][0]})" for p in sorted(stale))
        + "\nPlease remove them from RUNTIME_PYMESHLAB_ALLOWLIST in tests/test_no_runtime_pymeshlab.py."
    )

    # 3. Check that site counts did not regress
    for rel_path, (_owner, expected_count, _desc) in RUNTIME_PYMESHLAB_ALLOWLIST.items():
        actual_count = len(found.get(rel_path, []))
        assert actual_count <= expected_count, (
            f"{rel_path} has {actual_count} pymeshlab import sites, exceeding allowed {expected_count}."
        )


def test_allowlist_structure():
    """Verify metadata format of the allowlist."""
    for rel_path, (owner, count, desc) in RUNTIME_PYMESHLAB_ALLOWLIST.items():
        assert isinstance(rel_path, str) and not rel_path.startswith('/'), rel_path
        assert owner in ('W1', 'W2', 'W3', 'W4a', 'W1/W4a', 'W5'), f"Unknown owner: {owner}"
        assert isinstance(count, int) and count > 0, (rel_path, count)
        assert isinstance(desc, str) and len(desc) > 0, rel_path


if __name__ == '__main__':
    test_no_unauthorized_runtime_pymeshlab_imports()
    print(f"OK: {len(RUNTIME_PYMESHLAB_ALLOWLIST)} allowlisted files with pymeshlab imports.")
    test_allowlist_structure()
    print("OK: allowlist structure valid.")
