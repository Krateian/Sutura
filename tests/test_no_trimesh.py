#!/usr/bin/env python3
"""Guard against trimesh creeping back into the runtime.

trimesh is no longer a Sutura runtime dependency: mesh IO and the pure-Python
geometry helpers live in ``sutura/`` itself (``sutura_engine.pipeline.mesh_io``,
``sutura_engine.hull``, ``sutura_engine.wall``, ...).  This guard scans
Sutura's own runtime sources only (``sutura/``); developer scripts under
``scripts/`` and the test suite may still use trimesh through
``requirements-dev.txt``.
"""
import ast
import importlib
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)


def _trimesh_imports(path):
    with open(path, 'r', encoding='utf-8') as f:
        tree = ast.parse(f.read(), filename=path)
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name == 'trimesh' or alias.name.startswith('trimesh.'):
                    yield node.lineno, alias.name
        elif isinstance(node, ast.ImportFrom):
            module = node.module or ''
            if module == 'trimesh' or module.startswith('trimesh.'):
                yield node.lineno, module


_SKIP_DIRS = {'__pycache__', '.git', 'build', 'dist', 'node_modules'}


def _is_third_party(name):
    return name == 'site-packages' or name.startswith('venv')


def _python_files(root):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames
                       if d not in _SKIP_DIRS and not _is_third_party(d)]
        for name in filenames:
            if name.endswith('.py'):
                yield os.path.join(dirpath, name)


def test_runtime_has_no_trimesh_imports():
    offenders = []
    for path in _python_files(os.path.join(REPO, 'sutura')):
        for lineno, name in _trimesh_imports(path):
            offenders.append('%s:%s %s' % (os.path.relpath(path, REPO),
                                           lineno, name))
    assert not offenders, 'trimesh imports remain:\n' + '\n'.join(offenders)


def test_runtime_imports_without_trimesh():
    class _BlockTrimesh:
        def find_spec(self, fullname, path=None, target=None):
            if fullname == 'trimesh' or fullname.startswith('trimesh.'):
                raise ImportError('trimesh is intentionally unavailable')
            return None

    blocker = _BlockTrimesh()
    sys.meta_path.insert(0, blocker)
    try:
        for name in ('sutura_engine.wall', 'sutura_engine.dressing',
                     'sutura_engine.mirror', 'sutura_engine.hull',
                     'sutura_engine.graft',
                     'sutura_engine.methods.repeat',
                     'sutura_engine.methods.proxy'):
            importlib.import_module(name)
    finally:
        sys.meta_path.remove(blocker)


if __name__ == '__main__':
    test_runtime_has_no_trimesh_imports()
    test_runtime_imports_without_trimesh()
    print('no-trimesh guard passed')
