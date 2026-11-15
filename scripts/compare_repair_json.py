#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
"""Compare two `repair --json` reports (Python vs Rust pipeline).

Normalises timing fields and absolute paths, compares the remaining tree
recursively in key order, and prints the first differing path. Exit 1 on a
difference, 2 on a parse error.
"""
import argparse
import json
import os
import re
import sys

TIME_KEYS = {'seconds', 'timings', 'elapsed_ms', 'time_ms', 'duration_ms'}
TIME_SUFFIXES = ('_s', '_ms', '_seconds', '_ns', '_us')
ABS_RE = re.compile(r'^(?:/|[A-Za-z]:[\\/]).*')


def _norm_key(key):
    return key in TIME_KEYS or key.endswith(TIME_SUFFIXES)


def _norm_string(value):
    if not isinstance(value, str):
        return value
    if not ABS_RE.match(value) and not os.path.isabs(value):
        return value
    base = os.path.basename(value.rstrip('/\\'))
    return '<path>/' + base if base else '<path>'


def normalise(node):
    if isinstance(node, dict):
        out = {}
        for key in sorted(node):
            if _norm_key(key):
                continue
            out[key] = normalise(node[key])
        return out
    if isinstance(node, list):
        return [normalise(x) for x in node]
    return _norm_string(node)


def first_diff(a, b, path='$'):
    if type(a) is not type(b):
        return path, a, b
    if isinstance(a, dict):
        for key in sorted(set(a) | set(b)):
            if key not in a:
                return path + '.' + key, '<missing>', b[key]
            if key not in b:
                return path + '.' + key, a[key], '<missing>'
            found = first_diff(a[key], b[key], path + '.' + key)
            if found:
                return found
        return None
    if isinstance(a, list):
        if len(a) != len(b):
            return path + '.len', len(a), len(b)
        for i, (x, y) in enumerate(zip(a, b)):
            found = first_diff(x, y, '%s[%d]' % (path, i))
            if found:
                return found
        return None
    if a != b:
        return path, a, b
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('python_json')
    ap.add_argument('rust_json')
    args = ap.parse_args()
    try:
        a = normalise(json.load(open(args.python_json)))
        b = normalise(json.load(open(args.rust_json)))
    except Exception as exc:  # noqa: BLE001
        print('parse error: %s' % exc, file=sys.stderr)
        return 2
    found = first_diff(a, b)
    if not found:
        print('SAME')
        return 0
    path, left, right = found
    print('DIFF %s' % path)
    print('  python: %r' % (left,))
    print('  rust:   %r' % (right,))
    return 1


if __name__ == '__main__':
    raise SystemExit(main())
