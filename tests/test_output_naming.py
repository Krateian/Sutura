#!/usr/bin/env python3
"""C7: auto output names never overwrite an existing _fixed file."""
import hashlib
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPAIR = os.path.join(REPO, 'sutura', 'repair.py')
MAKE = os.path.join(REPO, 'tests', 'make_broken_stl.py')


def _sha(path):
    return hashlib.sha256(open(path, 'rb').read()).hexdigest()


def _run(args):
    return subprocess.run([sys.executable, REPAIR, '--no-history', '--no-cache']
                          + args, capture_output=True, text=True, timeout=300)


def main():
    tmp = tempfile.mkdtemp(prefix='sutura-outname-')
    src = os.path.join(tmp, 'part.stl')
    subprocess.run([sys.executable, MAKE, src], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    first = os.path.join(tmp, 'part_fixed.stl')
    second = os.path.join(tmp, 'part_fixed_2.stl')
    r = _run([src])
    assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
    assert os.path.exists(first), r.stderr
    h1 = _sha(first)
    r = _run([src])
    assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
    assert os.path.exists(second), r.stderr
    assert _sha(first) == h1, 'the first _fixed file was overwritten'
    print('ok  _fixed then _fixed_2, first unchanged')

    explicit = os.path.join(tmp, 'explicit.stl')
    for _ in range(2):
        r = _run(['-o', explicit, src])
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
    assert os.path.exists(explicit)
    assert not os.path.exists(os.path.join(tmp, 'explicit_2.stl'))
    print('ok  explicit -o keeps its exact path')


if __name__ == '__main__':
    main()
    print('output naming tests passed')
