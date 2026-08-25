#!/usr/bin/env python3
"""OBJ repair regression tests for sutura/repair.py.

Checks that:
  1. A broken OBJ (same defect set as make_broken_stl.py) repairs to a
     two-manifold mesh with a _fixed.obj output and material_discarded=False.
  2. An OBJ with mtllib/usemtl references reports material_discarded=True
     (JSON) and shows the Material: line (--human), and the category is NOT
     downgraded by the cosmetic warning (the repair still succeeds).
  3. A lone mtllib or usemtl line each trigger the warning.

Uses the repo's own repair.py under the venv (pymeshlab), not the installed
CLI, so this tracks the code under test.
Usage: ~/.local/share/sutura/venv/bin/python tests/test_obj_repair.py
"""
import json
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPAIR_PY = os.path.join(REPO, 'sutura', 'repair.py')
MAKER = os.path.join(REPO, 'tests', 'make_broken_obj.py')

# make the flat sutura modules importable (repair.py does top-level
# `from classification import ...`), so the helper can be tested directly
sys.path.insert(0, os.path.join(REPO, 'sutura'))
from repair import obj_has_material_refs  # noqa: E402


def _run_repair(path, env):
    return subprocess.run([sys.executable, REPAIR_PY, path],
                          capture_output=True, text=True, env=env)


def test_broken_obj_repairs():
    with tempfile.TemporaryDirectory(prefix='sutura-obj-') as tmp:
        src = os.path.join(tmp, 'broken.obj')
        subprocess.run([sys.executable, MAKER, src], check=True)
        env = dict(os.environ, SUTURA_DIR=tmp)
        p = _run_repair(src, env)
        assert p.returncode == 0, p.stdout + p.stderr
        fixed = os.path.join(tmp, 'broken_fixed.obj')
        assert os.path.exists(fixed), 'no _fixed.obj output'
        d = json.loads(p.stdout.strip().splitlines()[-1])
        # the broken cube repairs to a closed mesh; category is watertight or
        # warning depending on whether the stage-2 bridge exists in this env
        assert d['category'] != 'error', d.get('category')
        assert d['stage1']['two_manifold'] is True
        assert d['stage1']['holes_remaining'] == 0
        assert d.get('material_discarded') is False
        # the fixed OBJ must still be a valid mesh
        assert os.path.getsize(fixed) > 100


def _repair_json(path, env):
    p = _run_repair(path, env)
    assert p.returncode == 0, p.stdout + p.stderr
    return json.loads(p.stdout.strip().splitlines()[-1])


def test_material_references_flagged():
    with tempfile.TemporaryDirectory(prefix='sutura-obj-') as tmp:
        src = os.path.join(tmp, 'broken_mat.obj')
        subprocess.run([sys.executable, MAKER, src, '--material'], check=True)
        env = dict(os.environ, SUTURA_DIR=tmp)
        d = _repair_json(src, env)
        # cosmetic warning: the material flag must NOT change the category
        # vs the plain broken OBJ (same geometry); it only adds a field
        plain = os.path.join(tmp, 'broken.obj')
        subprocess.run([sys.executable, MAKER, plain], check=True)
        d_plain = _repair_json(plain, env)
        assert d['category'] == d_plain['category'], (d['category'],
                                                      d_plain['category'])
        assert d_plain.get('material_discarded') is False
        assert d.get('material_discarded') is True
        assert 'material_discarded' not in d.get('issues', [])


def test_human_report_shows_material_line():
    with tempfile.TemporaryDirectory(prefix='sutura-obj-') as tmp:
        src = os.path.join(tmp, 'broken_mat.obj')
        subprocess.run([sys.executable, MAKER, src, '--material'], check=True)
        env = dict(os.environ, SUTURA_DIR=tmp)
        p = subprocess.run([sys.executable, REPAIR_PY, src, '--human'],
                           capture_output=True, text=True, env=env)
        assert 'Material:' in p.stdout, p.stdout
        assert 'not preserved' in p.stdout


def test_mtllib_alone_and_usemtl_alone():
    with tempfile.TemporaryDirectory(prefix='sutura-obj-') as tmp:
        m = os.path.join(tmp, 'm.obj')
        with open(m, 'w') as f:
            f.write('mtllib foo.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n')
        assert obj_has_material_refs(m) is True
        with open(m, 'w') as f:
            f.write('v 0 0 0\nusemtl bar\nf 1 2 3\n')
        assert obj_has_material_refs(m) is True
        with open(m, 'w') as f:
            f.write('v 0 0 0\nf 1 2 3\n')
        assert obj_has_material_refs(m) is False


def main():
    tests = [(n, f) for n, f in sorted(globals().items())
             if n.startswith('test_') and callable(f)]
    failed = 0
    for name, fn in tests:
        try:
            fn()
            print('ok  %s' % name)
        except AssertionError as e:
            failed += 1
            print('FAIL %s: %s' % (name, e))
    if failed:
        print('%d/%d tests failed' % (failed, len(tests)))
        sys.exit(1)
    print('all %d tests passed' % len(tests))


if __name__ == '__main__':
    main()