#!/usr/bin/env python3
"""Capture and compare repair --json goldens under report_schema.json.

Capture stores scrubbed reports (paths become <mesh:NNNN>, timings ignored) so
private paths/names never enter a tracked file. Comparison follows the
schema classes: structural exact, integer exact, rounded floats exact at their
declared decimals, raw floats with the documented tolerance.
"""
import argparse
import fnmatch
import json
import os
import re
import subprocess
import sys
import tempfile

SCHEMA = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      'rust', 'sutura-core', 'report_schema.json')
DETERMINISTIC = ['--no-graft', '--no-fallback-ftetwild', '--deep-repair', 'off',
                 '--no-history', '--no-cache', '--json']


def load_schema(path=SCHEMA):
    with open(path) as f:
        return json.load(f)


def norm_path(parts):
    return '.'.join('[]' if isinstance(p, int) else p for p in parts)


def class_for(schema, parts):
    path = norm_path(parts)
    for pat in schema.get('ignore_paths', []):
        if fnmatch.fnmatchcase(path, pat):
            return 'ignore', None
    for item in schema.get('nested', []):
        if fnmatch.fnmatchcase(path, item['path']):
            return item['class'], item.get('decimals')
    if len(parts) == 1:
        return schema.get('top_level', {}).get(parts[0]), None
    return None, None


def is_num(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def compare_value(a, b, parts, schema, diffs, where=''):
    cls, decimals = class_for(schema, parts)
    path = where or norm_path(parts) or '<root>'
    if cls == 'ignore':
        return
    if a is None or b is None:
        if not (a is None and b is None):
            diffs.append('%s: null mismatch %r/%r' % (path, a, b))
        return
    if cls is None:
        cls = 'D' if (is_num(a) or is_num(b)) else 'A'
    if cls == 'A':
        if type(a) is not type(b):
            diffs.append('%s: type %s != %s' % (path, type(a).__name__, type(b).__name__))
            return
        if isinstance(a, dict):
            if set(a) != set(b):
                diffs.append('%s: keys %s != %s' % (path, sorted(a), sorted(b)))
                return
            for k in a:
                compare_value(a[k], b[k], parts + [k], schema, diffs, path + '.' + k)
        elif isinstance(a, list):
            if len(a) != len(b):
                diffs.append('%s: length %d != %d' % (path, len(a), len(b)))
                return
            for i, (x, y) in enumerate(zip(a, b)):
                compare_value(x, y, parts + [i], schema, diffs, path + '[]')
        elif a != b:
            diffs.append('%s: %r != %r' % (path, a, b))
    elif cls == 'B':
        if not (isinstance(a, int) and isinstance(b, int) and not isinstance(a, bool)
                and not isinstance(b, bool)):
            diffs.append('%s: expected integers, got %r/%r' % (path, a, b))
        elif a != b:
            diffs.append('%s: %d != %d' % (path, a, b))
    elif cls == 'C':
        if not (is_num(a) and is_num(b)):
            diffs.append('%s: expected numbers, got %r/%r' % (path, a, b))
            return
        n = decimals if decimals is not None else 6
        if round(float(a), n) != round(float(b), n):
            diffs.append('%s: round(%r,%d) != round(%r,%d)' % (path, a, n, b, n))
    elif cls == 'D':
        if not (is_num(a) and is_num(b)):
            diffs.append('%s: expected numbers, got %r/%r' % (path, a, b))
            return
        tol = schema['tolerance']['abs'] + schema['tolerance']['rel'] * max(
            1.0, abs(float(a)), abs(float(b)))
        if abs(float(a) - float(b)) > tol:
            diffs.append('%s: %r != %r (tol %g)' % (path, a, b, tol))
    else:
        diffs.append('%s: unknown class %r' % (path, cls))


def scrub(obj, index, in_path, out_path):
    old_in, old_out = os.path.abspath(in_path), os.path.abspath(out_path)
    placeholder = '<mesh:%04d>' % index
    if isinstance(obj, str):
        s = obj.replace(old_in, placeholder).replace(old_out, placeholder)
        s = s.replace(os.path.dirname(old_in), '<input-dir>')
        s = s.replace(os.path.dirname(old_out), '<output-dir>')
        return s
    if isinstance(obj, list):
        return [scrub(v, index, in_path, out_path) for v in obj]
    if isinstance(obj, dict):
        return {k: scrub(v, index, in_path, out_path) for k, v in obj.items()}
    return obj


def capture(args):
    recs = []
    for i, path in enumerate(args.files):
        out = tempfile.mktemp(suffix='.stl')
        cmd = [args.python, os.path.join(args.repo, 'sutura', 'repair.py'), path,
               '-o', out] + DETERMINISTIC
        p = subprocess.run(cmd, capture_output=True, text=True,
                           cwd=args.repo)
        if p.returncode != 0:
            raise SystemExit('repair failed for mesh %04d: %s' %
                             (i, p.stderr.strip()[-300:]))
        try:
            report = json.loads(p.stdout)
        except json.JSONDecodeError as exc:
            raise SystemExit('repair did not emit JSON for mesh %04d: %s' % (i, exc))
        recs.append({'index': i, 'report': scrub(report, i, path, out)})
        try:
            os.unlink(out)
        except OSError:
            pass
    payload = {'schema_version': load_schema()['version'], 'meshes': recs}
    with open(args.out, 'w') as f:
        json.dump(payload, f, indent=1, sort_keys=False)
    print('wrote %s (%d meshes)' % (args.out, len(recs)))


def compare(args):
    schema = load_schema(args.schema)
    a = json.load(open(args.a))
    b = json.load(open(args.b))
    diffs = []
    if len(a.get('meshes', [])) != len(b.get('meshes', [])):
        diffs.append('mesh count %d != %d' % (len(a.get('meshes', [])),
                                             len(b.get('meshes', []))))
    for ma, mb in zip(a.get('meshes', []), b.get('meshes', [])):
        if ma.get('index') != mb.get('index'):
            diffs.append('mesh index %r != %r' % (ma.get('index'), mb.get('index')))
            continue
        compare_value(ma.get('report'), mb.get('report'), [], schema, diffs,
                      'mesh[%04d]' % ma.get('index', -1))
    if diffs:
        print('DIFF %d' % len(diffs))
        for d in diffs[:40]:
            print('  ' + d)
        return 1
    print('SAME')
    return 0


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest='cmd', required=True)
    c = sub.add_parser('capture')
    c.add_argument('--python', required=True)
    c.add_argument('--repo', required=True)
    c.add_argument('--out', required=True)
    c.add_argument('files', nargs='+')
    c.set_defaults(func=capture)
    cmp = sub.add_parser('compare')
    cmp.add_argument('a')
    cmp.add_argument('b')
    cmp.add_argument('--schema', default=SCHEMA)
    cmp.set_defaults(func=compare)
    a = ap.parse_args()
    sys.exit(a.func(a))


if __name__ == '__main__':
    main()
