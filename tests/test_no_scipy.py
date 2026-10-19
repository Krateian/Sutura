#!/usr/bin/env python3
"""Guard against SciPy creeping back into the runtime.

SciPy is no longer a Sutura dependency.  The only allowed import is the
research-only alpha-wrap prototype under ``scripts/experiments``.
"""
import ast
import importlib
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)


def _scipy_imports(path):
    with open(path, 'r', encoding='utf-8') as f:
        tree = ast.parse(f.read(), filename=path)
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name == 'scipy' or alias.name.startswith('scipy.'):
                    yield node.lineno, alias.name
        elif isinstance(node, ast.ImportFrom):
            module = node.module or ''
            if module == 'scipy' or module.startswith('scipy.'):
                yield node.lineno, module


def _python_files(root):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d != '__pycache__']
        for name in filenames:
            if name.endswith('.py'):
                yield os.path.join(dirpath, name)


def test_runtime_has_no_scipy_imports():
    allowed = {os.path.join(REPO, 'scripts', 'experiments',
                            'alpha_wrap_prototype.py')}
    offenders = []
    for root in (os.path.join(REPO, 'sutura'), os.path.join(REPO, 'scripts')):
        for path in _python_files(root):
            if path in allowed:
                continue
            for lineno, name in _scipy_imports(path):
                offenders.append('%s:%s %s' % (os.path.relpath(path, REPO),
                                               lineno, name))
    assert not offenders, 'SciPy imports remain:\n' + '\n'.join(offenders)


def test_runtime_imports_without_scipy():
    class _BlockSciPy:
        def find_spec(self, fullname, path=None, target=None):
            if fullname == 'scipy' or fullname.startswith('scipy.'):
                raise ImportError('scipy is intentionally unavailable')
            return None

    blocker = _BlockSciPy()
    sys.meta_path.insert(0, blocker)
    try:
        for name in ('sutura_engine.wall', 'sutura_engine.dressing',
                     'sutura_engine.mirror', 'sutura_engine.hull',
                     'sutura_engine.methods.repeat',
                     'sutura_engine.methods.proxy'):
            importlib.import_module(name)
    finally:
        sys.meta_path.remove(blocker)


if __name__ == '__main__':
    test_runtime_has_no_scipy_imports()
    test_runtime_imports_without_scipy()
    print('no-scipy guard passed')
