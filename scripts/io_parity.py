#!/usr/bin/env python3
"""Compare the Python mesh_io path with the Rust `sutura io` path.

Rows are numeric summaries only; temporary meshes are deleted and never
uploaded. The verdict is SAME only when the per-block array hashes, unit
warnings, scan result and the written STL bytes agree.
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile


def run(cmd, env=None):
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


def sha_file(path):
    with open(path, 'rb') as f:
        return hashlib.sha256(f.read()).hexdigest()


def summary(side):
    meshes = side.get('meshes') or []
    return {
        'format': side.get('format'),
        'meshes': len(meshes),
        'verts': sum(m.get('verts', 0) for m in meshes),
        'tris': sum(m.get('tris', 0) for m in meshes),
        'array_sha256': [m.get('array_sha256') for m in meshes],
        'write_sha256': side.get('write_sha256'),
        'unit_warnings': side.get('unit_warnings') or [],
        'material_refs': side.get('material_refs'),
        'scan': side.get('scan'),
    }


def compare(a, b):
    diffs = []
    for key in ('format', 'meshes', 'verts', 'tris', 'array_sha256',
                'write_sha256', 'unit_warnings', 'material_refs', 'scan'):
        if a.get(key) != b.get(key):
            diffs.append(key)
    return diffs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('files', nargs='+')
    ap.add_argument('--baseline-repo', required=True)
    ap.add_argument('--baseline-python', required=True)
    ap.add_argument('--rust-bin', required=True)
    ap.add_argument('--out-dir', required=True)
    a = ap.parse_args()
    os.makedirs(a.out_dir, exist_ok=True)
    probe = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'io_probe.py')
    rows = []
    for path in a.files:
        name = os.path.basename(path)
        tmp = tempfile.mkdtemp(prefix='io-parity-')
        try:
            py_stl = os.path.join(tmp, 'py.stl')
            rs_stl = os.path.join(tmp, 'rs.stl')
            env = dict(os.environ)
            env['PYTHONPATH'] = os.path.join(a.baseline_repo, 'sutura')
            p = run([a.baseline_python, probe, path, '--write-stl', py_stl], env=env)
            if p.returncode != 0:
                rows.append({'id': name, 'name': name, 'verdict': 'ERROR',
                             'reason': 'python probe rc=%d: %s' %
                                       (p.returncode, p.stderr.strip()[-160:]),
                             'baseline': {}, 'candidate': {}})
                continue
            py = json.loads(p.stdout)
            r = run([a.rust_bin, 'io', 'probe', path, '--json'])
            if r.returncode != 0:
                rows.append({'id': name, 'name': name, 'verdict': 'ERROR',
                             'reason': 'rust probe rc=%d: %s' %
                                       (r.returncode, r.stderr.strip()[-160:]),
                             'baseline': summary(py), 'candidate': {}})
                continue
            rs = json.loads(r.stdout)
            w = run([a.rust_bin, 'io', 'convert', path, rs_stl])
            if w.returncode != 0:
                rows.append({'id': name, 'name': name, 'verdict': 'ERROR',
                             'reason': 'rust convert rc=%d: %s' %
                                       (w.returncode, w.stderr.strip()[-160:]),
                             'baseline': summary(py), 'candidate': summary(rs)})
                continue
            rs['write_sha256'] = sha_file(rs_stl)
            base, cand = summary(py), summary(rs)
            diffs = compare(base, cand)
            rows.append({'id': name, 'name': name,
                         'verdict': 'SAME' if not diffs else 'DIFF',
                         'reason': None if not diffs else 'differs: ' + ', '.join(diffs),
                         'baseline': base, 'candidate': cand})
        except Exception as exc:
            rows.append({'id': name, 'name': name, 'verdict': 'ERROR',
                         'reason': '%s: %s' % (type(exc).__name__, exc),
                         'baseline': {}, 'candidate': {}})
        finally:
            shutil.rmtree(tmp, ignore_errors=True)
    with open(os.path.join(a.out_dir, 'summary.json'), 'w') as f:
        json.dump(rows, f, indent=1)
    print(json.dumps(rows, indent=1))
    return 0 if all(r['verdict'] == 'SAME' for r in rows) else 1


if __name__ == '__main__':
    sys.exit(main())
