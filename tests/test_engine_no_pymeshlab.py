# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Test that sutura_engine core, analysis, and triage import cleanly without pymeshlab."""
import subprocess
import sys
import os

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def test_import_engine_without_pymeshlab():
    code = (
        "import sys\n"
        f"sys.path.insert(0, {REPO!r})\n"
        "sys.modules['pymeshlab'] = None\n"
        "import sutura_engine.core\n"
        "import sutura_engine.analysis\n"
        "import sutura_engine.triage\n"
        "print('engine modules imported successfully without pymeshlab')\n"
    )
    r = subprocess.run([sys.executable, '-c', code], capture_output=True, text=True)
    assert r.returncode == 0, f"Import failed without pymeshlab:\nSTDOUT:\n{r.stdout}\nSTDERR:\n{r.stderr}"
    assert 'engine modules imported successfully' in r.stdout


if __name__ == '__main__':
    test_import_engine_without_pymeshlab()
    print('ok test_import_engine_without_pymeshlab')
