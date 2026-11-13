#!/usr/bin/env python3
"""Aggregate shard JSONs into the board; --check fails on worse/fail/missing.

The verdicts follow .claude/skills/sutura-verify/scripts/
verify_default_vs_optout.py: TRADE-SI (SI-after rose while every visible
shape metric stayed equal or better) is its own non-failing verdict, listed
on a separate line with its SI delta, exactly like the local board.  WORSE,
FAIL, ERROR, CHANGED, MISSING and VISUAL-WORSE fail.

Every shape column is baseline/candidate.  The two-sided distance is shown
twice: the visible value (the judge's gate, input-alone interior walls
excluded) and the ``*_all_*`` value that also counts those walls.
"""
import argparse
import glob
import json
import sys

FAILING = ('WORSE', 'FAIL', 'ERROR', 'CHANGED', 'MISSING', 'VISUAL-WORSE')
COLS = [
    ('Holes b/c', 'reload_holes', '%s'),
    ('NM b/c', 'reload_non_manifold_edges', '%s'),
    ('SI b/c', 'si_output', '%s'),
    ('2-sided max b/c', 'two_sided_max_mm', '%.4f'),
    ('2-sided p95 b/c', 'two_sided_p95_mm', '%.4f'),
    ('2-sided max all b/c', 'two_sided_max_all_mm', '%.4f'),
    ('2-sided p95 all b/c', 'two_sided_p95_all_mm', '%.4f'),
    ('i2o all b/c', 'i2o_max_all_mm', '%.4f'),
    ('Vol err b/c', 'volume_error_rel', '%.6f'),
    ('Parts b/c', None, '%s'),
    ('Shells b/c', 'closed_shells', '%s'),
    ('Leak mm b/c', 'leak_max_mm', '%.4f'),
    ('Leak pts b/c', 'leak_count', '%s'),
]


def _fmt(v, f):
    if v is None:
        return '-'
    try:
        return f % v
    except TypeError:
        return str(v)


def _parts(m):
    parts = m.get('orca_number_of_parts')
    return parts if parts is not None else m.get('components')


def _md5s(b, c):
    mb, mc = b.get('md5'), c.get('md5')
    if not mb and not mc:
        return '-'
    return '%s/%s' % (str(mb or '-')[:8], str(mc or '-')[:8])


def _strict_cell(row, side):
    """Strict-watertight for one side, from the shard or recomputed.

    Older shard files predate the field, so recompute from the reloaded
    holes/non-manifold counts rather than showing a blank.
    """
    val = (row.get('strict_watertight') or {}).get(side)
    if val is None:
        rec = row.get(side) or {}
        holes, nm = rec.get('reload_holes'), rec.get('reload_non_manifold_edges')
        if holes is None or nm is None:
            return '-'
        val = (holes == 0 and nm == 0)
    return 'yes' if val else 'no'


def _cell(b, c, key, f):
    if key is None:
        return '%s/%s' % (_fmt(_parts(b), f), _fmt(_parts(c), f))
    return '%s/%s' % (_fmt(b.get(key), f), _fmt(c.get(key), f))


def _si_trade(b, c, r):
    if r.get('verdict') != 'TRADE-SI':
        return '-'
    if b.get('si_output') is None or c.get('si_output') is None:
        return '-'
    return '+%d' % (int(c['si_output']) - int(b['si_output']))


def _plan_errors(plan, files):
    """Shards the workflow planned but which uploaded no summary.

    ``aggregate`` runs with ``if: always()``, so a shard that timed out or
    crashed simply leaves no JSON behind.  Without this check its meshes would
    vanish from the board instead of failing it.
    """
    expected = set()
    for shard in (plan.get('include') or []):
        expected.add((shard['set'], shard['mode'], int(shard['shard'])))
    seen = set()
    for d in files:
        seen.add((d.get('set'), d.get('mode'), int(d.get('shard', -1))))
    return ['no summary for set %s mode %s shard %d' % t
            for t in sorted(expected - seen)]


SEPTEMBER_STRICT = {'bench115': '102/115 at c0f5d26, v0.2.8 era'}


def _strict_summary(rows):
    """Per-set strict-watertight counts, September-comparable, never gated.

    The counts are printed for comparison with
    docs/repair-benchmark-strict-watertight-2026-09.md.  They are not part of
    the failing set: September measured a different pipeline era, and a count
    is a measurement, not an acceptance decision.
    """
    from collections import defaultdict
    agg = defaultdict(lambda: {'baseline': 0, 'candidate': 0, 'measured': 0,
                               'skipped': 0, 'error': 0})
    for r in rows:
        if not r.get('strict_watertight'):
            continue
        a = agg[r.get('set') or '-']
        a['measured'] += 1
        if r['strict_watertight'].get('baseline'):
            a['baseline'] += 1
        if r['strict_watertight'].get('candidate'):
            a['candidate'] += 1
        if r.get('verdict') == 'SKIPPED-NONDET':
            a['skipped'] += 1
        if r.get('verdict') == 'ERROR':
            a['error'] += 1
    if not agg:
        return ''
    lines = ['', 'Strict watertight (defects.detect on the reloaded output; '
                 'September-comparable, printed not gated):']
    for name in sorted(agg):
        a = agg[name]
        n = a['measured']
        line = ('  %-12s baseline %d/%d, candidate %d/%d'
                % (name, a['baseline'], n, a['candidate'], n))
        if a['skipped']:
            line += '  (SKIPPED-NONDET %d)' % a['skipped']
        if a['error']:
            line += '  (ERROR %d)' % a['error']
        ref = SEPTEMBER_STRICT.get(name)
        if ref:
            line += '  [September 2026: %s]' % ref
        lines.append(line)
    return '\n'.join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('dir')
    ap.add_argument('--check', action='store_true')
    ap.add_argument('--plan', default=None,
                    help='preflight matrix JSON; any planned shard without a '
                         'summary is an error')
    a = ap.parse_args()
    rows, errors, docs = [], [], []
    for p in sorted(glob.glob(a.dir + '/*.json')):
        d = json.load(open(p))
        docs.append(d)
        if d.get('error'):
            errors.append(d['error'])
        for r in d.get('rows', []):
            r = dict(r)
            r.setdefault('set', d.get('set'))
            r.setdefault('mode', d.get('mode'))
            rows.append(r)
    if a.plan:
        plan = json.load(open(a.plan))
        errors.extend(_plan_errors(plan, docs))
        for shard in plan.get('include') or []:
            match = [d for d in docs
                     if d.get('set') == shard['set']
                     and d.get('mode') == shard['mode']
                     and int(d.get('shard', -1)) == int(shard['shard'])]
            if match and match[0].get('meshes') != len(match[0].get('rows') or []):
                errors.append('set %s shard %s: %s meshes but %d rows'
                              % (shard['set'], shard['shard'],
                                 match[0].get('meshes'),
                                 len(match[0].get('rows') or [])))
    bad = [r for r in rows
           if str(r.get('verdict', '')).upper() in FAILING]
    trades = [r for r in rows if r.get('verdict') == 'TRADE-SI']
    print('| set | mode | public ID | verdict | SI trade | MD5 b/c | StrictWT b/c | %s | note |'
          % ' | '.join(h for h, _k, _f in COLS))
    print('|---|---|---|---|--:|---|--:|%s|---|' % '|'.join('---' for _ in COLS))
    for r in sorted(rows, key=lambda x: (str(x.get('set')), str(x.get('mode')),
                                         str(x.get('id')))):
        b = r.get('baseline') or {}
        c = r.get('candidate') or {}
        strict = '%s/%s' % (_strict_cell(r, 'baseline'),
                            _strict_cell(r, 'candidate'))
        print('| %s | %s | %s | %s | %s | %s | %s | %s | %s |' % (
            r.get('set') or '-', r.get('mode') or '-', r['id'], r['verdict'],
            _si_trade(b, c, r), _md5s(b, c), strict,
            ' | '.join(_cell(b, c, k, f) for _h, k, f in COLS),
            str(r.get('reason') or '-').replace('|', '/')))
    print(_strict_summary(rows))
    print('\nRows: %d; failing: %d; TRADE-SI: %d; shard errors: %d'
          % (len(rows), len(bad), len(trades), len(errors)))
    if trades:
        print('TRADE-SI (approved SI-for-shape trade, non-blocking): %s'
              % '; '.join('%s (%s)' % (r['id'], r.get('reason', ''))
                          for r in trades))
    if errors:
        print('shard errors: %s' % '; '.join(str(e)[:200] for e in errors))
    if a.check and (bad or errors):
        sys.exit(1)


if __name__ == '__main__':
    main()
