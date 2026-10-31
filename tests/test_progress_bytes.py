#!/usr/bin/env python3
"""C8: progress must not change output bytes or work counters."""
import hashlib
import json
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPAIR = os.path.join(REPO, 'sutura', 'repair.py')
SAMPLES = os.path.join(REPO, 'tests', 'real-world-samples')
MESHES = ['thingi10k_100827.stl', 'thingi10k_1038441.stl',
          'artec_metal-nut.stl', 'thingi10k_100281.stl']
FLAGS = ['--json', '--no-history', '--no-cache', '--deep-repair', 'off',
         '--no-graft', '--no-flap', '--no-tamponade', '--no-dressing',
         '--no-coaptation', '--no-splint', '--no-fallback-ftetwild']


def _sha(path):
    return hashlib.sha256(open(path, 'rb').read()).hexdigest()


def _work_units(obj, out):
    if isinstance(obj, dict):
        for k, v in obj.items():
            if k in ('work_units', 'work_budget', 'grid_work_units',
                     'fallback_work_units', 'fallback_work_budget'):
                out.append((k, v))
            _work_units(v, out)
    elif isinstance(obj, list):
        for v in obj:
            _work_units(v, out)


def _run(src, out, progress):
    args = [sys.executable, REPAIR, src, '-o', out] + FLAGS
    if progress:
        args.append('--progress-json')
    r = subprocess.run(args, capture_output=True, text=True, timeout=900)
    assert r.returncode == 0, (r.returncode, r.stdout[-2000:], r.stderr[-2000:])
    return json.loads(r.stdout), r.stderr


def main():
    tmp = tempfile.mkdtemp(prefix='sutura-progress-bytes-')
    for name in MESHES:
        src = os.path.join(SAMPLES, name)
        assert os.path.exists(src), src
        a = os.path.join(tmp, name + '.plain.stl')
        b = os.path.join(tmp, name + '.prog.stl')
        ja, _ = _run(src, a, False)
        jb, err = _run(src, b, True)
        assert 'progress-json' not in err
        assert '"event": "stage"' in err, err[-500:]
        assert _sha(a) == _sha(b), name
        wa, wb = [], []
        _work_units(ja, wa)
        _work_units(jb, wb)
        assert wa == wb, (name, wa, wb)
        print('ok ', name, len(wa), 'work fields, bytes equal')
    print('progress byte/work-unit test passed')


if __name__ == '__main__':
    main()
