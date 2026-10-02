# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Test that sutura_engine imports cleanly without pymeshlab.

Also covers the repository-root bootstrap package (`sutura_engine/__init__.py`),
which makes `import sutura_engine` work from the repo root without the former
symlink, and from `sutura/` directly.
"""
import subprocess
import sys
import os

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')


def test_import_engine_without_pymeshlab():
    code = (
        "import sys\n"
        f"sys.path.insert(0, {SUTURA!r})\n"
        "sys.modules['pymeshlab'] = None\n"
        "import sutura_engine.core\n"
        "import sutura_engine.diagnosis\n"
        "import sutura_engine.chart\n"
        "import sutura_engine.triage\n"
        "import sutura_engine.analysis\n"      # backward-compat shim
        "print('engine modules imported successfully without pymeshlab')\n"
    )
    r = subprocess.run([sys.executable, '-c', code], capture_output=True, text=True)
    assert r.returncode == 0, f"Import failed without pymeshlab:\nSTDOUT:\n{r.stdout}\nSTDERR:\n{r.stderr}"
    assert 'engine modules imported successfully' in r.stdout


def test_root_bootstrap_imports_from_repo_root():
    """`import sutura_engine` (and a submodule) works from the repo root even
    though the engine package physically lives under sutura/."""
    code = (
        "import sys\n"
        f"sys.path.insert(0, {REPO!r})\n"
        "import sutura_engine\n"
        "import sutura_engine.methods as m\n"
        "assert len(m.all_methods()) == 16, m.all_methods()\n"
        "assert hasattr(sutura_engine, '__path__')\n"
        "print('root bootstrap import OK')\n"
    )
    r = subprocess.run([sys.executable, '-c', code], capture_output=True, text=True,
                       cwd=REPO)
    assert r.returncode == 0, f"Root import failed:\nSTDOUT:\n{r.stdout}\nSTDERR:\n{r.stderr}"
    assert 'root bootstrap import OK' in r.stdout


def test_import_from_sutura_directory():
    code = (
        "import sutura_engine\n"
        "assert hasattr(sutura_engine, 'repair')\n"
        "print('sutura-dir import OK')\n"
    )
    r = subprocess.run([sys.executable, '-c', code], capture_output=True, text=True,
                       cwd=SUTURA)
    assert r.returncode == 0, f"sutura/ import failed:\nSTDOUT:\n{r.stdout}\nSTDERR:\n{r.stderr}"
    assert 'sutura-dir import OK' in r.stdout


if __name__ == '__main__':
    test_import_engine_without_pymeshlab()
    print('ok test_import_engine_without_pymeshlab')
    test_root_bootstrap_imports_from_repo_root()
    print('ok test_root_bootstrap_imports_from_repo_root')
    test_import_from_sutura_directory()
    print('ok test_import_from_sutura_directory')
